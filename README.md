# Polars
[![rust docs](https://docs.rs/polars/badge.svg)](https://docs.rs/polars/latest/polars/)
![Build and test](https://github.com/ritchie46/polars/workflows/Build%20and%20test/badge.svg)
[![](http://meritbadge.herokuapp.com/polars)](https://crates.io/crates/polars)
[![Gitter](https://badges.gitter.im/polars-rs/community.svg)](https://gitter.im/polars-rs/community?utm_source=badge&utm_medium=badge&utm_campaign=pr-badge)

## Blazingly fast DataFrames in Rust & Python

Polars is a blazingly fast DataFrames library implemented in Rust using Apache Arrow as memory model.

* Lazy | eager execution
* Multi-threaded
* SIMD
* Query optimization
* Powerful expression API
* Rust | Python | ...

To learn more, read the [User Guide](https://pola-rs.github.io/polars-book/).

# Rust users read this!
Polars cannot deploy a new version to `crates.io` until a new arrow release is issued. Arrow's release cycle takes 3/4
months which is a lot slower than I'd like to release. If it has been a while since a release is issued, it is recommended 
to use the current `master` branch instead of the published version on `crates.io`. 

You can add the master like this:

```toml
polars = {version="0.13.0", git = "https://github.com/ritchie46/polars" }
```

Or by fixing to a specific version:

```toml
polars = {version="0.13.0", git = "https://github.com/ritchie46/polars", rev = "<optional git tag>" } 
```
## Rust version
Required Rust version `>=1.51`

# Python users read this!
Polars is currently transitioning from `py-polars` to `polars`. Some docs may still refer the old name. 

Install the latest polars version with: 
`$ pip3 install polars`

## Documentation
Want to know about all the features Polars support? Read the docs!

#### Rust
* [Documentation (stable)](https://docs.rs/polars/latest/polars/). 
* [Documentation (master branch)](https://pola-rs.github.io/polars/polars/index.html). 
    * [DataFrame](https://pola-rs.github.io/polars/polars/frame/struct.DataFrame.html) 
    * [Series](https://pola-rs.github.io/polars/polars/prelude/struct.Series.html)
    * [ChunkedArray](https://pola-rs.github.io/polars/polars/chunked_array/struct.ChunkedArray.html)
    * [Traits for ChunkedArray](https://pola-rs.github.io/polars/polars/chunked_array/ops/index.html)
    * [Time/ DateTime utilities](https://pola-rs.github.io/polars/polars/doc/time/index.html)
    * [Groupby, aggregations and pivots](https://pola-rs.github.io/polars/polars/frame/groupby/struct.GroupBy.html)
    * [Lazy DataFrame](https://pola-rs.github.io/polars/polars/prelude/struct.LazyFrame.html)
* [User Guide](https://pola-rs.github.io/polars-book/)
    
#### Python
* installation guide: `$ pip3 install polars`
* [User Guide](https://pola-rs.github.io/polars-book/)
* [Reference guide](https://pola-rs.github.io/polars-book/api-python/)

## Performance
Polars is written to be performant, and it is! But don't take my word for it, take a look at the results in 
[h2oai's db-benchmark](https://h2oai.github.io/db-benchmark/).

## Cargo Features

Additional cargo features:

* `temporal (default)`
    - Conversions between Chrono and Polars for temporal data
* `simd (nightly)`
    - SIMD operations
* `parquet`
    - Read Apache Parquet format
* `json`
    - Json serialization
* `ipc`
    - Arrow's IPC format serialization
* `random`
    - Generate array's with randomly sampled values
* `ndarray`
    - Convert from `DataFrame` to `ndarray`
* `lazy`
    - Lazy api
* `strings`
    - String utilities for `Utf8Chunked`
* `object`
    - Support for generic ChunkedArray's called `ObjectChunked<T>` (generic over `T`). 
      These will downcastable from Series through the [Any](https://doc.rust-lang.org/std/any/index.html) trait.
* `[plain_fmt | pretty_fmt]` (mutually exclusive)
  - one of them should be chosen to fmt DataFrames. 
    `pretty_fmt` can deal with overflowing cells and looks nicer but has more dependencies.
    `plain_fmt (default)` is plain formatting.
  


## Contribution
Want to contribute? Read our [contribution guideline](https://github.com/ritchie46/polars/blob/master/CONTRIBUTING.md).


## ENV vars
* `POLARS_PAR_SORT_BOUND` -> Sets the lower bound of rows at which Polars will use a parallel sorting algorithm.
                             Default is 1M rows.
* `POLARS_FMT_MAX_COLS` -> maximum number of columns shown when formatting DataFrames.
* `POLARS_FMT_MAX_ROWS` -> maximum number of rows shown when formatting DataFrames.
* `POLARS_TABLE_WIDTH` -> width of the tables used during DataFrame formatting.
* `POLARS_MAX_THREADS` -> maximum number of threads used in join algorithm. Default is unbounded.
* `POLARS_VERBOSE` -> print logging info to stderr

## \[Python\] compile py-polars from source
If you want a bleeding edge release or maximal performance you should compile **py-polars** from source.

This can be done by going through the following steps in sequence:

1. install the latest [rust compiler](https://www.rust-lang.org/tools/install)
2. `$ pip3 install maturin`
4.  Choose any of:
  * Very long compile times, fastest binary: `$ cd py-polars && maturin develop --rustc-extra-args="-C target-cpu=native" --release`
  * Shorter compile times, fast binary: `$ cd py-polars && maturin develop --rustc-extra-args="-C codegen-units=16 lto=no target-cpu=native" --release`

Note that the Rust crate implementing the Python bindings is called `py-polars` to distinguish from the wrapped 
Rust crate `polars` itself. However, both the Python package and the Python module are named `polars`, so you
can `pip install polars` and `import polars` (previously, these were called `py-polars` and `pypolars`).

## Acknowledgements
Development of Polars is proudly powered by

[![Xomnia](https://raw.githubusercontent.com/ritchie46/img/master/polars/xomnia_logo.png)](https://www.xomnia.com)