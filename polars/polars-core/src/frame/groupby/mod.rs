use crate::chunked_array::builder::PrimitiveChunkedBuilder;
use crate::frame::select::Selection;
use crate::prelude::*;
use crate::utils::{accumulate_dataframes_vertical, split_ca, split_df, NoNull};
use crate::vector_hasher::{
    create_hash_and_keys_threaded_vectorized, df_rows_to_hashes, df_rows_to_hashes_threaded,
    prepare_hashed_relation, this_thread, IdBuildHasher, IdxHash,
};
use crate::POOL;
use ahash::RandomState;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use itertools::Itertools;
use rayon::prelude::*;
use std::fmt::Debug;
use std::hash::{BuildHasher, Hash};

pub mod aggregations;
#[cfg(feature = "pivot")]
pub(crate) mod pivot;
#[cfg(feature = "downsample")]
pub mod resample;

pub type GroupTuples = Vec<(u32, Vec<u32>)>;
pub type GroupedMap<T> = HashMap<T, Vec<u32>, RandomState>;

fn groupby<T>(a: impl Iterator<Item = T>) -> GroupTuples
where
    T: Hash + Eq,
{
    let hash_tbl = prepare_hashed_relation(a);

    hash_tbl
        .into_iter()
        .map(|(_, indexes)| {
            let first = unsafe { *indexes.get_unchecked(0) };
            (first, indexes)
        })
        .collect()
}

fn groupby_threaded_flat<I, T>(iters: Vec<I>, group_size_hint: usize) -> GroupTuples
where
    I: IntoIterator<Item = T> + Send,
    T: Send + Hash + Eq + Sync + Copy,
{
    groupby_threaded(iters, group_size_hint)
        .into_iter()
        .flatten()
        .collect()
}

/// Determine groupby tuples from an iterator. The group_size_hint is used to pre-allocate the group vectors.
/// When the grouping column is a categorical type we already have a good indication of the avg size of the groups.
fn groupby_threaded<I, T>(iters: Vec<I>, group_size_hint: usize) -> Vec<GroupTuples>
where
    I: IntoIterator<Item = T> + Send,
    T: Send + Hash + Eq + Sync + Copy,
{
    let n_threads = iters.len();
    let (hashes_and_keys, random_state) = create_hash_and_keys_threaded_vectorized(iters, None);
    let size = hashes_and_keys.iter().fold(0, |acc, v| acc + v.len());

    // We will create a hashtable in every thread.
    // We use the hash to partition the keys to the matching hashtable.
    // Every thread traverses all keys/hashes and ignores the ones that doesn't fall in that partition.
    POOL.install(|| {
        (0..n_threads).into_par_iter().map(|thread_no| {
            let random_state = random_state.clone();
            let hashes_and_keys = &hashes_and_keys;
            let thread_no = thread_no as u64;

            let mut hash_tbl: HashMap<T, (u32, Vec<u32>), RandomState> =
                HashMap::with_capacity_and_hasher(size / n_threads, random_state);

            let n_threads = n_threads as u64;
            let mut offset = 0;
            for hashes_and_keys in hashes_and_keys {
                let len = hashes_and_keys.len() as u32;
                hashes_and_keys
                    .iter()
                    .enumerate()
                    .for_each(|(idx, (h, k))| {
                        let idx = idx as u32;
                        // partition hashes by thread no.
                        // So only a part of the hashes go to this hashmap
                        if (h + thread_no) % n_threads == 0 {
                            let idx = idx + offset;
                            let entry = hash_tbl
                                .raw_entry_mut()
                                // uses the key to check equality to find and entry
                                .from_key_hashed_nocheck(*h, &k);

                            match entry {
                                RawEntryMut::Vacant(entry) => {
                                    let mut tuples = Vec::with_capacity(group_size_hint);
                                    tuples.push(idx);
                                    entry.insert_hashed_nocheck(*h, *k, (idx, tuples));
                                }
                                RawEntryMut::Occupied(mut entry) => {
                                    let (_k, v) = entry.get_key_value_mut();
                                    v.1.push(idx);
                                }
                            }
                        }
                    });

                offset += len;
            }
            hash_tbl.into_iter().map(|(_k, v)| v).collect::<Vec<_>>()
        })
    })
    .collect()
}

/// Utility function used as comparison function in the hashmap.
/// The rationale is that equality is an AND operation and therefore its probability of success
/// declines rapidly with the number of keys. Instead of first copying an entire row from both
/// sides and then do the comparison, we do the comparison value by value catching early failures
/// eagerly.
///
/// # Safety
/// Doesn't check any bounds
pub(crate) unsafe fn compare_df_rows(keys: &DataFrame, idx_a: usize, idx_b: usize) -> bool {
    for s in keys.get_columns() {
        if !s.equal_element(idx_a, idx_b, s) {
            return false;
        }
    }
    true
}

/// Populate a multiple key hashmap with row indexes.
/// Instead of the keys (which could be very large), the row indexes are stored.
/// To check if a row is equal the original DataFrame is also passed as ref.
/// When a hash collision occurs the indexes are ptrs to the rows and the rows are compared
/// on equality.
pub(crate) fn populate_multiple_key_hashmap<V, H, F, G>(
    hash_tbl: &mut HashMap<IdxHash, V, H>,
    // row index
    idx: u32,
    // hash
    h: u64,
    // keys of the hash table (will not be inserted, the indexes will be used)
    // the keys are needed for the equality check
    keys: &DataFrame,
    // value to insert
    vacant_fn: G,
    // function that gets a mutable ref to the occupied value in the hash table
    occupied_fn: F,
) where
    G: Fn() -> V,
    F: Fn(&mut V),
    H: BuildHasher,
{
    let entry = hash_tbl
        .raw_entry_mut()
        // uses the idx to probe rows in the original DataFrame with keys
        // to check equality to find an entry
        .from_hash(h, |idx_hash| {
            let key_idx = idx_hash.idx;
            // Safety:
            // indices in a groupby operation are always in bounds.
            unsafe { compare_df_rows(keys, key_idx as usize, idx as usize) }
        });
    match entry {
        RawEntryMut::Vacant(entry) => {
            entry.insert_hashed_nocheck(h, IdxHash::new(idx, h), vacant_fn());
        }
        RawEntryMut::Occupied(mut entry) => {
            let (_k, v) = entry.get_key_value_mut();
            occupied_fn(v);
        }
    }
}

fn groupby_multiple_keys(keys: DataFrame) -> GroupTuples {
    let (hashes, _) = df_rows_to_hashes(&keys, None);
    let size = hashes.len();
    // rather over allocate because rehashing is expensive
    let mut hash_tbl: HashMap<IdxHash, (u32, Vec<u32>), IdBuildHasher> =
        HashMap::with_capacity_and_hasher(size, IdBuildHasher::default());

    // hashes has no nulls
    let mut idx = 0;
    for hashes_chunk in hashes.data_views() {
        for &h in hashes_chunk {
            populate_multiple_key_hashmap(
                &mut hash_tbl,
                idx,
                h,
                &keys,
                || (idx, vec![idx]),
                |v| v.1.push(idx),
            );
            idx += 1;
        }
    }
    hash_tbl.into_iter().map(|(_k, v)| v).collect::<Vec<_>>()
}

fn groupby_threaded_multiple_keys_flat(keys: DataFrame, n_threads: usize) -> GroupTuples {
    let dfs = split_df(&keys, n_threads).unwrap();
    let (hashes, _random_state) = df_rows_to_hashes_threaded(&dfs, None);
    let size = hashes.len();

    // We will create a hashtable in every thread.
    // We use the hash to partition the keys to the matching hashtable.
    // Every thread traverses all keys/hashes and ignores the ones that doesn't fall in that partition.

    // We use a combination of a custom IdentityHasher and a utility key IdxHash that stores
    // the index of the row and and the hash. The Hash function of this key just returns the hash it stores.
    POOL.install(|| {
        (0..n_threads).into_par_iter().map(|thread_no| {
            let hashes = &hashes;
            let thread_no = thread_no as u64;

            let keys = &keys;

            // rather over allocate because rehashing is expensive
            let mut hash_tbl: HashMap<IdxHash, (u32, Vec<u32>), IdBuildHasher> =
                HashMap::with_capacity_and_hasher(size / n_threads, IdBuildHasher::default());

            let n_threads = n_threads as u64;
            let mut offset = 0;
            for hashes in hashes {
                let len = hashes.len() as u32;

                let mut idx = 0;
                for hashes_chunk in hashes.data_views() {
                    for &h in hashes_chunk {
                        // partition hashes by thread no.
                        // So only a part of the hashes go to this hashmap
                        if this_thread(h, thread_no, n_threads) {
                            let idx = idx + offset;
                            populate_multiple_key_hashmap(
                                &mut hash_tbl,
                                idx,
                                h,
                                &keys,
                                || (idx, vec![idx]),
                                |v| v.1.push(idx),
                            );
                        }
                        idx += 1;
                    }
                }

                offset += len;
            }
            hash_tbl.into_iter().map(|(_k, v)| v).collect::<Vec<_>>()
        })
    })
    .flatten()
    .collect()
}

/// Used to create the tuples for a groupby operation.
pub trait IntoGroupTuples {
    /// Create the tuples need for a groupby operation.
    ///     * The first value in the tuple is the first index of the group.
    ///     * The second value in the tuple is are the indexes of the groups including the first value.
    fn group_tuples(&self, _multithreaded: bool) -> GroupTuples {
        unimplemented!()
    }
}

fn group_multithreaded<T>(ca: &ChunkedArray<T>) -> bool {
    // TODO! change to something sensible
    ca.len() > 1000
}

macro_rules! group_tuples {
    ($ca: expr, $multithreaded: expr) => {{
        // TODO! choose a splitting len
        if $multithreaded && group_multithreaded($ca) {
            let n_threads = num_cpus::get();
            let splitted = split_ca($ca, n_threads).unwrap();

            if $ca.null_count() == 0 {
                let iters = splitted
                    .iter()
                    .map(|ca| ca.into_no_null_iter())
                    .collect_vec();
                groupby_threaded_flat(iters, 0)
            } else {
                let iters = splitted.iter().map(|ca| ca.into_iter()).collect_vec();
                groupby_threaded_flat(iters, 0)
            }
        } else {
            if $ca.null_count() == 0 {
                groupby($ca.into_no_null_iter())
            } else {
                groupby($ca.into_iter())
            }
        }
    }};
}

impl<T> IntoGroupTuples for ChunkedArray<T>
where
    T: PolarsIntegerType,
    T::Native: Eq + Hash + Send,
{
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        let group_size_hint = if let Some(m) = &self.categorical_map {
            self.len() / m.len()
        } else {
            0
        };
        if multithreaded && group_multithreaded(self) {
            let n_threads = num_cpus::get();
            let splitted = split_ca(self, n_threads).unwrap();

            // use the arrays as iterators
            if self.chunks.len() == 1 {
                if self.null_count() == 0 {
                    let iters = splitted
                        .iter()
                        .map(|ca| ca.downcast_iter().map(|array| array.values()))
                        .flatten()
                        .collect_vec();
                    groupby_threaded_flat(iters, group_size_hint)
                } else {
                    let iters = splitted
                        .iter()
                        .map(|ca| ca.downcast_iter())
                        .flatten()
                        .collect_vec();
                    groupby_threaded_flat(iters, group_size_hint)
                }
                // use the polars-iterators
            } else if self.null_count() == 0 {
                let iters = splitted
                    .iter()
                    .map(|ca| ca.into_no_null_iter())
                    .collect_vec();
                groupby_threaded_flat(iters, group_size_hint)
            } else {
                let iters = splitted.iter().map(|ca| ca.into_iter()).collect_vec();
                groupby_threaded_flat(iters, group_size_hint)
            }
        } else if self.null_count() == 0 {
            groupby(self.into_no_null_iter())
        } else {
            groupby(self.into_iter())
        }
    }
}
impl IntoGroupTuples for BooleanChunked {
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        group_tuples!(self, multithreaded)
    }
}

impl IntoGroupTuples for Utf8Chunked {
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        group_tuples!(self, multithreaded)
    }
}

impl IntoGroupTuples for CategoricalChunked {
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        self.cast::<UInt32Type>()
            .unwrap()
            .group_tuples(multithreaded)
    }
}

macro_rules! impl_into_group_tpls_float {
    ($self: ident, $multithreaded:expr) => {
        if $multithreaded && group_multithreaded($self) {
            let n_threads = num_cpus::get();
            let splitted = split_ca($self, n_threads).unwrap();
            match $self.null_count() {
                0 => {
                    let iters = splitted
                        .iter()
                        .map(|ca| ca.into_no_null_iter().map(|v| v.to_bits()))
                        .collect_vec();
                    groupby_threaded_flat(iters, 0)
                }
                _ => {
                    let iters = splitted
                        .iter()
                        .map(|ca| ca.into_iter().map(|opt_v| opt_v.map(|v| v.to_bits())))
                        .collect_vec();
                    groupby_threaded_flat(iters, 0)
                }
            }
        } else {
            match $self.null_count() {
                0 => groupby($self.into_no_null_iter().map(|v| v.to_bits())),
                _ => groupby($self.into_iter().map(|opt_v| opt_v.map(|v| v.to_bits()))),
            }
        }
    };
}

impl IntoGroupTuples for Float64Chunked {
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        impl_into_group_tpls_float!(self, multithreaded)
    }
}
impl IntoGroupTuples for Float32Chunked {
    fn group_tuples(&self, multithreaded: bool) -> GroupTuples {
        impl_into_group_tpls_float!(self, multithreaded)
    }
}
impl IntoGroupTuples for ListChunked {}
#[cfg(feature = "object")]
impl<T> IntoGroupTuples for ObjectChunked<T> {}

impl DataFrame {
    pub fn groupby_with_series(&self, by: Vec<Series>, multithreaded: bool) -> Result<GroupBy> {
        if by.is_empty() || by[0].len() != self.height() {
            return Err(PolarsError::ShapeMisMatch(
                "the Series used as keys should have the same length as the DataFrame".into(),
            ));
        };

        // make sure that categorical is used as uint32 in value type
        let keys_df = DataFrame::new(
            by.iter()
                .map(|s| match s.dtype() {
                    DataType::Categorical => s.cast::<UInt32Type>().unwrap(),
                    _ => s.clone(),
                })
                .collect(),
        )?;

        let groups = match by.len() {
            1 => {
                let series = &by[0];
                series.group_tuples(multithreaded)
            }
            _ => {
                if multithreaded {
                    let n_threads = num_cpus::get();
                    groupby_threaded_multiple_keys_flat(keys_df, n_threads)
                } else {
                    groupby_multiple_keys(keys_df)
                }
            }
        };
        Ok(GroupBy::new(self, by, groups, None))
    }

    /// Group DataFrame using a Series column.
    ///
    /// # Example
    ///
    /// ```
    /// use polars_core::prelude::*;
    /// fn groupby_sum(df: &DataFrame) -> Result<DataFrame> {
    ///     df.groupby("column_name")?
    ///     .select("agg_column_name")
    ///     .sum()
    /// }
    /// ```
    pub fn groupby<'g, J, S: Selection<'g, J>>(&self, by: S) -> Result<GroupBy> {
        let selected_keys = self.select_series(by)?;
        self.groupby_with_series(selected_keys, true)
    }

    /// Group DataFrame using a Series column.
    /// The groups are ordered by their smallest row index.
    pub fn groupby_stable<'g, J, S: Selection<'g, J>>(&self, by: S) -> Result<GroupBy> {
        let mut gb = self.groupby(by)?;
        gb.groups.sort();
        Ok(gb)
    }
}

/// Returned by a groupby operation on a DataFrame. This struct supports
/// several aggregations.
///
/// Until described otherwise, the examples in this struct are performed on the following DataFrame:
///
/// ```rust
/// use polars_core::prelude::*;
///
/// let dates = &[
/// "2020-08-21",
/// "2020-08-21",
/// "2020-08-22",
/// "2020-08-23",
/// "2020-08-22",
/// ];
/// // date format
/// let fmt = "%Y-%m-%d";
/// // create date series
/// let s0 = Date32Chunked::parse_from_str_slice("date", dates, fmt)
///         .into_series();
/// // create temperature series
/// let s1 = Series::new("temp", [20, 10, 7, 9, 1].as_ref());
/// // create rain series
/// let s2 = Series::new("rain", [0.2, 0.1, 0.3, 0.1, 0.01].as_ref());
/// // create a new DataFrame
/// let df = DataFrame::new(vec![s0, s1, s2]).unwrap();
/// println!("{:?}", df);
/// ```
///
/// Outputs:
///
/// ```text
/// +------------+------+------+
/// | date       | temp | rain |
/// | ---        | ---  | ---  |
/// | date32     | i32  | f64  |
/// +============+======+======+
/// | 2020-08-21 | 20   | 0.2  |
/// +------------+------+------+
/// | 2020-08-21 | 10   | 0.1  |
/// +------------+------+------+
/// | 2020-08-22 | 7    | 0.3  |
/// +------------+------+------+
/// | 2020-08-23 | 9    | 0.1  |
/// +------------+------+------+
/// | 2020-08-22 | 1    | 0.01 |
/// +------------+------+------+
/// ```
///
#[derive(Debug, Clone)]
pub struct GroupBy<'df, 'selection_str> {
    df: &'df DataFrame,
    pub(crate) selected_keys: Vec<Series>,
    // [first idx, [other idx]]
    pub(crate) groups: GroupTuples,
    // columns selected for aggregation
    pub(crate) selected_agg: Option<Vec<&'selection_str str>>,
}

impl<'df, 'selection_str> GroupBy<'df, 'selection_str> {
    pub fn new(
        df: &'df DataFrame,
        by: Vec<Series>,
        groups: GroupTuples,
        selected_agg: Option<Vec<&'selection_str str>>,
    ) -> Self {
        GroupBy {
            df,
            selected_keys: by,
            groups,
            selected_agg,
        }
    }

    /// Select the column(s) that should be aggregated.
    /// You can select a single column or a slice of columns.
    ///
    /// Note that making a selection with this method is not required. If you
    /// skip it all columns (except for the keys) will be selected for aggregation.
    pub fn select<S, J>(mut self, selection: S) -> Self
    where
        S: Selection<'selection_str, J>,
    {
        self.selected_agg = Some(selection.to_selection_vec());
        self
    }

    /// Get the internal representation of the GroupBy operation.
    /// The Vec returned contains:
    ///     (first_idx, Vec<indexes>)
    ///     Where second value in the tuple is a vector with all matching indexes.
    pub fn get_groups(&self) -> &GroupTuples {
        &self.groups
    }

    /// Get the internal representation of the GroupBy operation.
    /// The Vec returned contains:
    ///     (first_idx, Vec<indexes>)
    ///     Where second value in the tuple is a vector with all matching indexes.
    pub fn get_groups_mut(&mut self) -> &mut GroupTuples {
        &mut self.groups
    }

    pub fn keys(&self) -> Vec<Series> {
        // Keys will later be appended with the aggregation columns, so we already allocate extra space
        let size;
        if let Some(sel) = &self.selected_agg {
            size = sel.len() + self.selected_keys.len();
        } else {
            size = self.selected_keys.len();
        }
        let mut keys = Vec::with_capacity(size);
        unsafe {
            self.selected_keys.iter().for_each(|s| {
                let key =
                    s.take_iter_unchecked(&mut self.groups.iter().map(|(idx, _)| *idx as usize));
                keys.push(key)
            });
        }
        keys
    }

    fn prepare_agg(&self) -> Result<(Vec<Series>, Vec<Series>)> {
        let selection = match &self.selected_agg {
            Some(selection) => selection.clone(),
            None => {
                let by: Vec<_> = self.selected_keys.iter().map(|s| s.name()).collect();
                self.df
                    .get_column_names()
                    .into_iter()
                    .filter(|a| !by.contains(a))
                    .collect()
            }
        };

        let keys = self.keys();
        let agg_col = self.df.select_series(selection)?;
        Ok((keys, agg_col))
    }

    /// Aggregate grouped series and compute the mean per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select(&["temp", "rain"]).mean()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+-----------+-----------+
    /// | date       | temp_mean | rain_mean |
    /// | ---        | ---       | ---       |
    /// | date32     | f64       | f64       |
    /// +============+===========+===========+
    /// | 2020-08-23 | 9         | 0.1       |
    /// +------------+-----------+-----------+
    /// | 2020-08-22 | 4         | 0.155     |
    /// +------------+-----------+-----------+
    /// | 2020-08-21 | 15        | 0.15      |
    /// +------------+-----------+-----------+
    /// ```
    pub fn mean(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;

        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Mean);
            let opt_agg = agg_col.agg_mean(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg);
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped series and compute the sum per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").sum()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+----------+
    /// | date       | temp_sum |
    /// | ---        | ---      |
    /// | date32     | i32      |
    /// +============+==========+
    /// | 2020-08-23 | 9        |
    /// +------------+----------+
    /// | 2020-08-22 | 8        |
    /// +------------+----------+
    /// | 2020-08-21 | 30       |
    /// +------------+----------+
    /// ```
    pub fn sum(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;

        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Sum);
            let opt_agg = agg_col.agg_sum(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg);
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped series and compute the minimal value per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").min()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+----------+
    /// | date       | temp_min |
    /// | ---        | ---      |
    /// | date32     | i32      |
    /// +============+==========+
    /// | 2020-08-23 | 9        |
    /// +------------+----------+
    /// | 2020-08-22 | 1        |
    /// +------------+----------+
    /// | 2020-08-21 | 10       |
    /// +------------+----------+
    /// ```
    pub fn min(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Min);
            let opt_agg = agg_col.agg_min(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg);
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped series and compute the maximum value per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").max()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+----------+
    /// | date       | temp_max |
    /// | ---        | ---      |
    /// | date32     | i32      |
    /// +============+==========+
    /// | 2020-08-23 | 9        |
    /// +------------+----------+
    /// | 2020-08-22 | 7        |
    /// +------------+----------+
    /// | 2020-08-21 | 20       |
    /// +------------+----------+
    /// ```
    pub fn max(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Max);
            let opt_agg = agg_col.agg_max(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg);
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and find the first value per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").first()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+------------+
    /// | date       | temp_first |
    /// | ---        | ---        |
    /// | date32     | i32        |
    /// +============+============+
    /// | 2020-08-23 | 9          |
    /// +------------+------------+
    /// | 2020-08-22 | 7          |
    /// +------------+------------+
    /// | 2020-08-21 | 20         |
    /// +------------+------------+
    /// ```
    pub fn first(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::First);
            let mut agg = agg_col.agg_first(&self.groups);
            agg.rename(&new_name);
            cols.push(agg);
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and return the last value per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").last()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+------------+
    /// | date       | temp_last |
    /// | ---        | ---        |
    /// | date32     | i32        |
    /// +============+============+
    /// | 2020-08-23 | 9          |
    /// +------------+------------+
    /// | 2020-08-22 | 1          |
    /// +------------+------------+
    /// | 2020-08-21 | 10         |
    /// +------------+------------+
    /// ```
    pub fn last(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Last);
            let mut agg = agg_col.agg_last(&self.groups);
            agg.rename(&new_name);
            cols.push(agg);
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` by counting the number of unique values.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").n_unique()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+---------------+
    /// | date       | temp_n_unique |
    /// | ---        | ---           |
    /// | date32     | u32           |
    /// +============+===============+
    /// | 2020-08-23 | 1             |
    /// +------------+---------------+
    /// | 2020-08-22 | 2             |
    /// +------------+---------------+
    /// | 2020-08-21 | 2             |
    /// +------------+---------------+
    /// ```
    pub fn n_unique(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::NUnique);
            let opt_agg = agg_col.agg_n_unique(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg.into_series());
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and determine the quantile per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").quantile(0.2)
    /// }
    /// ```
    pub fn quantile(&self, quantile: f64) -> Result<DataFrame> {
        if !(0.0..=1.0).contains(&quantile) {
            return Err(PolarsError::Other(
                "quantile should be within 0.0 and 1.0".into(),
            ));
        }
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Quantile(quantile));
            let opt_agg = agg_col.agg_quantile(&self.groups, quantile);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg.into_series());
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and determine the median per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").median()
    /// }
    /// ```
    pub fn median(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Median);
            let opt_agg = agg_col.agg_median(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg.into_series());
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and determine the variance per group.
    pub fn var(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Var);
            let opt_agg = agg_col.agg_var(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg.into_series());
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped `Series` and determine the standard deviation per group.
    pub fn std(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Std);
            let opt_agg = agg_col.agg_std(&self.groups);
            if let Some(mut agg) = opt_agg {
                agg.rename(&new_name);
                cols.push(agg.into_series());
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate grouped series and compute the number of values per group.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.select("temp").count()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+------------+
    /// | date       | temp_count |
    /// | ---        | ---        |
    /// | date32     | u32        |
    /// +============+============+
    /// | 2020-08-23 | 1          |
    /// +------------+------------+
    /// | 2020-08-22 | 2          |
    /// +------------+------------+
    /// | 2020-08-21 | 2          |
    /// +------------+------------+
    /// ```
    pub fn count(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::Count);
            let mut builder =
                PrimitiveChunkedBuilder::<UInt32Type>::new(&new_name, self.groups.len());
            for (_first, idx) in &self.groups {
                builder.append_value(idx.len() as u32);
            }
            let ca = builder.finish();
            cols.push(ca.into_series())
        }
        DataFrame::new(cols)
    }

    /// Get the groupby group indexes.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     df.groupby("date")?.groups()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +--------------+------------+
    /// | date         | groups     |
    /// | ---          | ---        |
    /// | date32(days) | list [u32] |
    /// +==============+============+
    /// | 2020-08-23   | "[3]"      |
    /// +--------------+------------+
    /// | 2020-08-22   | "[2, 4]"   |
    /// +--------------+------------+
    /// | 2020-08-21   | "[0, 1]"   |
    /// +--------------+------------+
    /// ```
    pub fn groups(&self) -> Result<DataFrame> {
        let mut cols = self.keys();

        let mut column: ListChunked = self
            .groups
            .iter()
            .map(|(_first, idx)| {
                let ca: NoNull<UInt32Chunked> = idx.iter().map(|&v| v as u32).collect();
                ca.into_inner().into_series()
            })
            .collect();
        let new_name = fmt_groupby_column("", GroupByMethod::Groups);
        column.rename(&new_name);
        cols.push(column.into_series());
        DataFrame::new(cols)
    }

    /// Combine different aggregations on columns
    ///
    /// ## Operations
    ///
    /// * count
    /// * first
    /// * last
    /// * sum
    /// * min
    /// * max
    /// * mean
    /// * median
    ///
    /// # Example
    ///
    ///  ```rust
    ///  # use polars_core::prelude::*;
    ///  fn example(df: DataFrame) -> Result<DataFrame> {
    ///      df.groupby("date")?.agg(&[("temp", &["n_unique", "sum", "min"])])
    ///  }
    ///  ```
    ///  Returns:
    ///
    ///  ```text
    ///  +--------------+---------------+----------+----------+
    ///  | date         | temp_n_unique | temp_sum | temp_min |
    ///  | ---          | ---           | ---      | ---      |
    ///  | date32(days) | u32           | i32      | i32      |
    ///  +==============+===============+==========+==========+
    ///  | 2020-08-23   | 1             | 9        | 9        |
    ///  +--------------+---------------+----------+----------+
    ///  | 2020-08-22   | 2             | 8        | 1        |
    ///  +--------------+---------------+----------+----------+
    ///  | 2020-08-21   | 2             | 30       | 10       |
    ///  +--------------+---------------+----------+----------+
    ///  ```
    ///
    pub fn agg<Column, S, Slice>(&self, column_to_agg: &[(Column, Slice)]) -> Result<DataFrame>
    where
        S: AsRef<str>,
        S: AsRef<str>,
        Slice: AsRef<[S]>,
        Column: AsRef<str>,
    {
        // create a mapping from columns to aggregations on that column
        let mut map = HashMap::with_capacity_and_hasher(column_to_agg.len(), RandomState::new());
        column_to_agg.iter().for_each(|(column, aggregations)| {
            map.insert(column.as_ref(), aggregations.as_ref());
        });

        macro_rules! finish_agg_opt {
            ($self:ident, $name_fmt:expr, $agg_fn:ident, $agg_col:ident, $cols:ident) => {{
                let new_name = format![$name_fmt, $agg_col.name()];
                let opt_agg = $agg_col.$agg_fn(&$self.groups);
                if let Some(mut agg) = opt_agg {
                    agg.rename(&new_name);
                    $cols.push(agg.into_series());
                }
            }};
        }
        macro_rules! finish_agg {
            ($self:ident, $name_fmt:expr, $agg_fn:ident, $agg_col:ident, $cols:ident) => {{
                let new_name = format![$name_fmt, $agg_col.name()];
                let mut agg = $agg_col.$agg_fn(&$self.groups);
                agg.rename(&new_name);
                $cols.push(agg.into_series());
            }};
        }

        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in &agg_cols {
            if let Some(&aggregations) = map.get(agg_col.name()) {
                for aggregation_f in aggregations {
                    match aggregation_f.as_ref() {
                        "min" => finish_agg_opt!(self, "{}_min", agg_min, agg_col, cols),
                        "max" => finish_agg_opt!(self, "{}_max", agg_max, agg_col, cols),
                        "mean" => finish_agg_opt!(self, "{}_mean", agg_mean, agg_col, cols),
                        "sum" => finish_agg_opt!(self, "{}_sum", agg_sum, agg_col, cols),
                        "first" => finish_agg!(self, "{}_first", agg_first, agg_col, cols),
                        "last" => finish_agg!(self, "{}_last", agg_last, agg_col, cols),
                        "n_unique" => {
                            finish_agg_opt!(self, "{}_n_unique", agg_n_unique, agg_col, cols)
                        }
                        "median" => finish_agg_opt!(self, "{}_median", agg_median, agg_col, cols),
                        "std" => finish_agg_opt!(self, "{}_std", agg_std, agg_col, cols),
                        "var" => finish_agg_opt!(self, "{}_var", agg_var, agg_col, cols),
                        "count" => {
                            let new_name = format!["{}_count", agg_col.name()];
                            let mut builder = PrimitiveChunkedBuilder::<UInt32Type>::new(
                                &new_name,
                                self.groups.len(),
                            );
                            for (_first, idx) in &self.groups {
                                builder.append_value(idx.len() as u32);
                            }
                            let ca = builder.finish();
                            cols.push(ca.into_series());
                        }
                        a => panic!("aggregation: {:?} is not supported", a),
                    }
                }
            }
        }
        DataFrame::new(cols)
    }

    /// Aggregate the groups of the groupby operation into lists.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use polars_core::prelude::*;
    /// fn example(df: DataFrame) -> Result<DataFrame> {
    ///     // GroupBy and aggregate to Lists
    ///     df.groupby("date")?.select("temp").agg_list()
    /// }
    /// ```
    /// Returns:
    ///
    /// ```text
    /// +------------+------------------------+
    /// | date       | temp_agg_list          |
    /// | ---        | ---                    |
    /// | date32     | list [i32]             |
    /// +============+========================+
    /// | 2020-08-23 | "[Some(9)]"            |
    /// +------------+------------------------+
    /// | 2020-08-22 | "[Some(7), Some(1)]"   |
    /// +------------+------------------------+
    /// | 2020-08-21 | "[Some(20), Some(10)]" |
    /// +------------+------------------------+
    /// ```
    pub fn agg_list(&self) -> Result<DataFrame> {
        let (mut cols, agg_cols) = self.prepare_agg()?;
        for agg_col in agg_cols {
            let new_name = fmt_groupby_column(agg_col.name(), GroupByMethod::List);
            if let Some(mut agg) = agg_col.agg_list(&self.groups) {
                agg.rename(&new_name);
                cols.push(agg);
            }
        }
        DataFrame::new(cols)
    }

    /// Apply a closure over the groups as a new DataFrame.
    pub fn apply<F>(&self, f: F) -> Result<DataFrame>
    where
        F: Fn(DataFrame) -> Result<DataFrame> + Send + Sync,
    {
        let df = if let Some(agg) = &self.selected_agg {
            if agg.is_empty() {
                self.df.clone()
            } else {
                let mut new_cols = Vec::with_capacity(self.selected_keys.len() + agg.len());
                new_cols.extend_from_slice(&self.selected_keys);
                let cols = self.df.select_series(agg)?;
                new_cols.extend(cols.into_iter());
                DataFrame::new_no_checks(new_cols)
            }
        } else {
            self.df.clone()
        };

        let dfs = self
            .get_groups()
            .par_iter()
            .map(|t| {
                let sub_df = unsafe { df.take_iter_unchecked(t.1.iter().map(|i| *i as usize)) };
                f(sub_df)
            })
            .collect::<Result<Vec<_>>>()?;

        let mut df = accumulate_dataframes_vertical(dfs)?;
        df.as_single_chunk();
        Ok(df)
    }
}

#[derive(Copy, Clone)]
pub enum GroupByMethod {
    Min,
    Max,
    Median,
    Mean,
    First,
    Last,
    Sum,
    Groups,
    NUnique,
    Quantile(f64),
    Count,
    List,
    Std,
    Var,
}

// Formatting functions used in eager and lazy code for renaming grouped columns
pub fn fmt_groupby_column(name: &str, method: GroupByMethod) -> String {
    use GroupByMethod::*;
    match method {
        Min => format!["{}_min", name],
        Max => format!["{}_max", name],
        Median => format!["{}_median", name],
        Mean => format!["{}_mean", name],
        First => format!["{}_first", name],
        Last => format!["{}_last", name],
        Sum => format!["{}_sum", name],
        Groups => "groups".to_string(),
        NUnique => format!["{}_n_unique", name],
        Count => format!["{}_count", name],
        List => format!["{}_agg_list", name],
        Quantile(quantile) => format!["{}_quantile_{:.2}", name, quantile],
        Std => format!["{}_agg_std", name],
        Var => format!["{}_agg_var", name],
    }
}

#[cfg(test)]
mod test {
    use itertools::Itertools;

    use crate::frame::groupby::{groupby, groupby_threaded_flat};
    use crate::prelude::*;
    use crate::utils::split_ca;

    #[test]
    #[cfg(feature = "dtype-date32")]
    fn test_group_by() {
        let s0 = Date32Chunked::parse_from_str_slice(
            "date",
            &[
                "2020-08-21",
                "2020-08-21",
                "2020-08-22",
                "2020-08-23",
                "2020-08-22",
            ],
            "%Y-%m-%d",
        )
        .into_series();
        let s1 = Series::new("temp", [20, 10, 7, 9, 1].as_ref());
        let s2 = Series::new("rain", [0.2, 0.1, 0.3, 0.1, 0.01].as_ref());
        let df = DataFrame::new(vec![s0, s1, s2]).unwrap();
        println!("{:?}", df);

        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").count().unwrap()
        );
        // Select multiple
        println!(
            "{:?}",
            df.groupby("date")
                .unwrap()
                .select(&["temp", "rain"])
                .mean()
                .unwrap()
        );
        // Group by multiple
        println!(
            "multiple keys {:?}",
            df.groupby(&["date", "temp"])
                .unwrap()
                .select("rain")
                .mean()
                .unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").sum().unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").min().unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").max().unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date")
                .unwrap()
                .select("temp")
                .agg_list()
                .unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").first().unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").last().unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date")
                .unwrap()
                .select("temp")
                .n_unique()
                .unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date")
                .unwrap()
                .select("temp")
                .quantile(0.2)
                .unwrap()
        );
        println!(
            "{:?}",
            df.groupby("date").unwrap().select("temp").median().unwrap()
        );
        // implicit select all and only aggregate on methods that support that aggregation
        let gb = df.groupby("date").unwrap().n_unique().unwrap();
        println!("{:?}", df.groupby("date").unwrap().n_unique().unwrap());
        // check the group by column is filtered out.
        assert_eq!(gb.width(), 2);
        println!(
            "{:?}",
            df.groupby("date")
                .unwrap()
                .agg(&[("temp", &["n_unique", "sum", "min"])])
                .unwrap()
        );
        println!("{:?}", df.groupby("date").unwrap().groups().unwrap());
    }

    #[test]
    fn test_static_groupby_by_12_columns() {
        // Build GroupBy DataFrame.
        let s0 = Series::new("G1", ["A", "A", "B", "B", "C"].as_ref());
        let s1 = Series::new("N", [1, 2, 2, 4, 2].as_ref());
        let s2 = Series::new("G2", ["k", "l", "m", "m", "l"].as_ref());
        let s3 = Series::new("G3", ["a", "b", "c", "c", "d"].as_ref());
        let s4 = Series::new("G4", ["1", "2", "3", "3", "4"].as_ref());
        let s5 = Series::new("G5", ["X", "Y", "Z", "Z", "W"].as_ref());
        let s6 = Series::new("G6", [false, true, true, true, false].as_ref());
        let s7 = Series::new("G7", ["r", "x", "q", "q", "o"].as_ref());
        let s8 = Series::new("G8", ["R", "X", "Q", "Q", "O"].as_ref());
        let s9 = Series::new("G9", [1, 2, 3, 3, 4].as_ref());
        let s10 = Series::new("G10", [".", "!", "?", "?", "/"].as_ref());
        let s11 = Series::new("G11", ["(", ")", "@", "@", "$"].as_ref());
        let s12 = Series::new("G12", ["-", "_", ";", ";", ","].as_ref());

        let df =
            DataFrame::new(vec![s0, s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12]).unwrap();
        println!("{:?}", df);

        let adf = df
            .groupby(&[
                "G1", "G2", "G3", "G4", "G5", "G6", "G7", "G8", "G9", "G10", "G11", "G12",
            ])
            .unwrap()
            .select("N")
            .sum()
            .unwrap();

        println!("{:?}", adf);

        assert_eq!(
            Vec::from(&adf.column("N_sum").unwrap().i32().unwrap().sort(false)),
            &[Some(1), Some(2), Some(2), Some(6)]
        );
    }

    #[test]
    fn test_dynamic_groupby_by_13_columns() {
        // The content for every groupby series.
        let series_content = ["A", "A", "B", "B", "C"];

        // The name of every groupby series.
        let series_names = [
            "G1", "G2", "G3", "G4", "G5", "G6", "G7", "G8", "G9", "G10", "G11", "G12", "G13",
        ];

        // Vector to contain every series.
        let mut series = Vec::with_capacity(14);

        // Create a series for every group name.
        for series_name in &series_names {
            let serie = Series::new(series_name, series_content.as_ref());
            series.push(serie);
        }

        // Create a series for the aggregation column.
        let serie = Series::new("N", [1, 2, 3, 3, 4].as_ref());
        series.push(serie);

        // Creat the dataframe with the computed series.
        let df = DataFrame::new(series).unwrap();
        println!("{:?}", df);

        // Compute the aggregated DataFrame by the 13 columns defined in `series_names`.
        let adf = df
            .groupby(&series_names)
            .unwrap()
            .select("N")
            .sum()
            .unwrap();
        println!("{:?}", adf);

        // Check that the results of the group-by are correct. The content of every column
        // is equal, then, the grouped columns shall be equal and in the same order.
        for series_name in &series_names {
            assert_eq!(
                Vec::from(&adf.column(series_name).unwrap().utf8().unwrap().sort(false)),
                &[Some("A"), Some("B"), Some("C")]
            );
        }

        // Check the aggregated column is the expected one.
        assert_eq!(
            Vec::from(&adf.column("N_sum").unwrap().i32().unwrap().sort(false)),
            &[Some(3), Some(4), Some(6)]
        );
    }

    #[test]
    fn test_groupby_floats() {
        let df = df! {"flt" => [1., 1., 2., 2., 3.],
                    "val" => [1, 1, 1, 1, 1]
        }
        .unwrap();
        let res = df.groupby("flt").unwrap().sum().unwrap();
        let res = res.sort("flt", false).unwrap();
        assert_eq!(
            Vec::from(res.column("val_sum").unwrap().i32().unwrap()),
            &[Some(2), Some(2), Some(1)]
        );
    }

    #[test]
    fn test_groupby_categorical() {
        let mut df = df! {"foo" => ["a", "a", "b", "b", "c"],
                    "ham" => ["a", "a", "b", "b", "c"],
                    "bar" => [1, 1, 1, 1, 1]
        }
        .unwrap();

        df.apply("foo", |s| s.cast::<CategoricalType>().unwrap())
            .unwrap();

        // check multiple keys and categorical
        let res = df
            .groupby_stable(&["foo", "ham"])
            .unwrap()
            .select("bar")
            .sum()
            .unwrap();

        assert_eq!(
            Vec::from(res.column("bar_sum").unwrap().i32().unwrap()),
            &[Some(2), Some(2), Some(1)]
        );
    }

    #[test]
    fn test_groupby_apply() {
        let df = df! {
            "a" => [1, 1, 2, 2, 2],
            "b" => [1, 2, 3, 4, 5]
        }
        .unwrap();

        let out = df.groupby("a").unwrap().apply(Ok).unwrap();
        assert!(out.sort("b", false).unwrap().frame_equal(&df));
    }

    #[test]
    fn test_groupby_threaded() {
        for slice in &[
            vec![1, 2, 3, 4, 4, 4, 2, 1],
            vec![1, 2, 3, 4, 4, 4, 2, 1, 1],
            vec![1, 2, 3, 4, 4, 4],
        ] {
            let ca = UInt8Chunked::new_from_slice("", &slice);
            let splitted = split_ca(&ca, 4).unwrap();

            let a = groupby(ca.into_iter()).into_iter().sorted().collect_vec();
            let b = groupby_threaded_flat(splitted.iter().map(|ca| ca.into_iter()).collect(), 0)
                .into_iter()
                .sorted()
                .collect_vec();

            assert_eq!(a, b);
        }
    }

    #[test]
    fn test_groupby_null_handling() -> Result<()> {
        let df = df!(
            "a" => ["a", "a", "a", "b", "b"],
            "b" => [Some(1), Some(2), None, None, Some(1)]
        )?;
        let out = df.groupby_stable("a")?.mean()?;

        assert_eq!(
            Vec::from(out.column("b_mean")?.f64()?),
            &[Some(1.5), Some(1.0)]
        );
        Ok(())
    }
}
