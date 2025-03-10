use super::*;
use crate::logical_plan::Context;
use crate::utils::rename_aexpr_root_name;
use polars_core::utils::{accumulate_dataframes_vertical, num_cpus, split_df};
use polars_core::POOL;
use rayon::prelude::*;

/// Take an input Executor and a multiple expressions
pub struct GroupByExec {
    input: Box<dyn Executor>,
    keys: Vec<Arc<dyn PhysicalExpr>>,
    aggs: Vec<Arc<dyn PhysicalExpr>>,
    apply: Option<Arc<dyn DataFrameUdf>>,
}

impl GroupByExec {
    pub(crate) fn new(
        input: Box<dyn Executor>,
        keys: Vec<Arc<dyn PhysicalExpr>>,
        aggs: Vec<Arc<dyn PhysicalExpr>>,
        apply: Option<Arc<dyn DataFrameUdf>>,
    ) -> Self {
        Self {
            input,
            keys,
            aggs,
            apply,
        }
    }
}

fn groupby_helper(
    df: DataFrame,
    keys: Vec<Series>,
    aggs: &[Arc<dyn PhysicalExpr>],
    apply: Option<&Arc<dyn DataFrameUdf>>,
    state: &ExecutionState,
) -> Result<DataFrame> {
    let gb = df.groupby_with_series(keys, true)?;
    if let Some(f) = apply {
        return gb.apply(|df| f.call_udf(df));
    }

    let groups = gb.get_groups();

    let mut columns = gb.keys();

    let agg_columns = POOL.install(|| {
        aggs
            .par_iter()
            .map(|expr| {
                let agg_expr = expr.as_agg_expr()?;
                let opt_agg = agg_expr.aggregate(&df, groups, state)?;
                if let Some(agg) = &opt_agg {
                    if agg.len() != groups.len() {
                        panic!(
                            "returned aggregation is a different length: {} than the group lengths: {}",
                            agg.len(),
                            groups.len()
                        )
                    }
                };
                Ok(opt_agg)
            })
            .collect::<Result<Vec<_>>>()
    })?;

    columns.extend(agg_columns.into_iter().flatten());

    let df = DataFrame::new_no_checks(columns);
    Ok(df)
}

impl Executor for GroupByExec {
    fn execute(&mut self, state: &ExecutionState) -> Result<DataFrame> {
        let df = self.input.execute(state)?;
        let keys = self
            .keys
            .iter()
            .map(|e| e.evaluate(&df, state))
            .collect::<Result<_>>()?;
        groupby_helper(df, keys, &self.aggs, self.apply.as_ref(), state)
    }
}

/// Take an input Executor and a multiple expressions
pub struct PartitionGroupByExec {
    input: Box<dyn Executor>,
    key: Arc<dyn PhysicalExpr>,
    phys_aggs: Vec<Arc<dyn PhysicalExpr>>,
    aggs: Vec<Expr>,
}

impl PartitionGroupByExec {
    pub(crate) fn new(
        input: Box<dyn Executor>,
        key: Arc<dyn PhysicalExpr>,
        phys_aggs: Vec<Arc<dyn PhysicalExpr>>,
        aggs: Vec<Expr>,
    ) -> Self {
        Self {
            input,
            key,
            phys_aggs,
            aggs,
        }
    }
}

fn run_partititions(
    df: &DataFrame,
    exec: &PartitionGroupByExec,
    state: &ExecutionState,
    n_threads: usize,
) -> Result<Vec<DataFrame>> {
    // We do a partitioned groupby.
    // Meaning that we first do the groupby operation arbitrarily
    // splitted on several threads. Than the final result we apply the same groupby again.
    let dfs = split_df(df, n_threads)?;

    POOL.install(|| {
        dfs.into_par_iter()
            .map(|df| {
                let key = exec.key.evaluate(&df, state)?;
                let phys_aggs = &exec.phys_aggs;
                let gb = df.groupby_with_series(vec![key], false)?;
                let groups = gb.get_groups();

                let mut columns = gb.keys();
                let agg_columns = phys_aggs
                    .par_iter()
                    .map(|expr| {
                        let agg_expr = expr.as_agg_expr()?;
                        let opt_agg = agg_expr.evaluate_partitioned(&df, groups, state)?;
                        if let Some(agg) = &opt_agg {
                            if agg[0].len() != groups.len() {
                                panic!(
                                    "returned aggregation is a different length: {} than the group lengths: {}",
                                    agg.len(),
                                    groups.len()
                                )
                            }
                        };
                        Ok(opt_agg)
                    }).collect::<Result<Vec<_>>>()?;

                columns.extend(agg_columns.into_iter().flatten().map(|v| v.into_iter()).flatten());

                let df = DataFrame::new_no_checks(columns);
                Ok(df)
            })
    }).collect()
}

#[allow(clippy::type_complexity)]
fn get_outer_agg_exprs(
    exec: &PartitionGroupByExec,
    df: &DataFrame,
) -> Result<(Vec<(Node, Arc<String>)>, Vec<Arc<dyn PhysicalExpr>>)> {
    // Due to the PARTITIONED GROUPBY the column names are be changed.
    // To make sure sure we can select the columns with the new names, we re-create the physical
    // aggregations with new root column names (being the output of the partitioned aggregation)j
    // We also keep a hold on the output names to rename the final aggregation.
    let mut expr_arena = Arena::with_capacity(32);
    let schema = df.schema();
    let aggs_and_names = exec
        .aggs
        .iter()
        .map(|e| {
            let out_field = e.to_field(&schema, Context::Aggregation)?;
            let out_name = Arc::new(out_field.name().clone());
            let node = to_aexpr(e.clone(), &mut expr_arena);
            rename_aexpr_root_name(node, &mut expr_arena, out_name.clone())?;
            Ok((node, out_name))
        })
        .collect::<Result<Vec<_>>>()?;

    let planner = DefaultPlanner {};

    let outer_phys_aggs = aggs_and_names
        .iter()
        .map(|(e, _)| planner.create_physical_expr(*e, Context::Aggregation, &mut expr_arena))
        .collect::<Result<Vec<_>>>()?;

    Ok((aggs_and_names, outer_phys_aggs))
}

fn sample_cardinality(key: &Series, sample_size: usize) -> f32 {
    let offset = (key.len() / 2) as i64;
    let s = key.slice(offset, sample_size);
    s.n_unique().unwrap() as f32 / s.len() as f32
}

impl Executor for PartitionGroupByExec {
    fn execute(&mut self, state: &ExecutionState) -> Result<DataFrame> {
        let original_df = self.input.execute(state)?;

        // already get the keys. This is the very last minute decision which groupby method we choose.
        // If the column is a categorical, we know the number of groups we have and can decide to continue
        // partitioned or go for the standard groupby. The partitioned is likely to be faster on a small number
        // of groups.
        let key = self.key.evaluate(&original_df, state)?;

        if std::env::var("POLARS_NO_PARTITION").is_ok() {
            if state.verbose {
                eprintln!("POLARS_NO_PARTITION set: running default HASH AGGREGATION")
            }
            return groupby_helper(original_df, vec![key], &self.phys_aggs, None, state);
        }

        let cardinality_frac = std::env::var("POLARS_PARTITION_CARDINALITY_FRAC")
            .map(|s| s.parse::<f32>().unwrap())
            .unwrap_or(0.1f32);

        let (frac, a) = if let Ok(ca) = key.categorical() {
            let cat_map = ca
                .get_categorical_map()
                .expect("categorical type has categorical_map");

            (cat_map.len() as f32 / ca.len() as f32, "known")
        } else {
            let sample_size = std::env::var("POLARS_PARTITION_SAMPLE_SIZE")
                .map(|s| s.parse::<usize>().unwrap())
                .unwrap_or(1250usize);
            (sample_cardinality(&key, sample_size), "estimated")
        };
        if state.verbose {
            eprintln!("{} cardinality: {}%", a, (frac * 100.0) as u32);
        }

        if frac > cardinality_frac {
            if state.verbose {
                eprintln!(
                    "estimated cardinality is > than allowed cardinality: {}\
                running default HASH AGGREGATION",
                    (cardinality_frac * 100.0) as u32
                );
            }
            return groupby_helper(original_df, vec![key], &self.phys_aggs, None, state);
        }
        if state.verbose {
            eprintln!("run PARTITIONED HASH AGGREGATION")
        }

        // Run the partitioned aggregations
        let n_threads = num_cpus::get();
        let dfs = run_partititions(&original_df, self, state, n_threads)?;

        // MERGE phase
        // merge and hash aggregate again
        let df = accumulate_dataframes_vertical(dfs)?;
        let key = self.key.evaluate(&df, state)?;

        let gb = df.groupby_with_series(vec![key], true)?;
        let groups = gb.get_groups();

        let (aggs_and_names, outer_phys_aggs) = get_outer_agg_exprs(self, &original_df)?;

        let mut columns = gb.keys();
        let agg_columns: Vec<_> = POOL.install(|| {
            outer_phys_aggs
                .par_iter()
                .zip(aggs_and_names.par_iter().map(|(_, name)| name))
                .filter_map(|(expr, name)| {
                    let agg_expr = expr.as_agg_expr().unwrap();
                    // If None the column doesn't exist anymore.
                    // For instance when summing a string this column will not be in the aggregation result
                    let opt_agg = agg_expr.evaluate_partitioned_final(&df, groups, state).ok();
                    opt_agg.map(|opt_s| {
                        opt_s.map(|mut s| {
                            s.rename(name);
                            s
                        })
                    })
                })
                .flatten()
                .collect()
        });

        columns.extend(agg_columns);

        let df = DataFrame::new_no_checks(columns);
        Ok(df)
    }
}
