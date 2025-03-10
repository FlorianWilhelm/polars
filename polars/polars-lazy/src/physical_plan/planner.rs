use super::expressions as phys_expr;
use crate::logical_plan::Context;
use crate::prelude::*;
use crate::utils::{aexpr_to_root_names, aexpr_to_root_nodes, agg_source_paths, has_aexpr};
use ahash::RandomState;
use itertools::Itertools;
use polars_core::prelude::*;
use polars_core::{frame::groupby::GroupByMethod, utils::parallel_op};
use polars_io::ScanAggregation;
use std::collections::HashSet;
use std::sync::Arc;

fn aggregate_expr_to_scan_agg(
    aggregate: Vec<Node>,
    expr_arena: &mut Arena<AExpr>,
) -> Vec<ScanAggregation> {
    aggregate
        .into_iter()
        .map(|mut expr| {
            let mut alias = None;
            if let AExpr::Alias(e, name) = expr_arena.get(expr) {
                expr = *e;
                alias = Some((**name).clone())
            };
            if let AExpr::Agg(agg) = expr_arena.get(expr) {
                match agg {
                    AAggExpr::Min(e) => ScanAggregation::Min {
                        column: (*aexpr_to_root_names(*e, expr_arena).pop().unwrap()).clone(),
                        alias,
                    },
                    AAggExpr::Max(e) => ScanAggregation::Max {
                        column: (*aexpr_to_root_names(*e, expr_arena).pop().unwrap()).clone(),
                        alias,
                    },
                    AAggExpr::Sum(e) => ScanAggregation::Sum {
                        column: (*aexpr_to_root_names(*e, expr_arena).pop().unwrap()).clone(),
                        alias,
                    },
                    AAggExpr::First(e) => ScanAggregation::First {
                        column: (*aexpr_to_root_names(*e, expr_arena).pop().unwrap()).clone(),
                        alias,
                    },
                    AAggExpr::Last(e) => ScanAggregation::Last {
                        column: (*aexpr_to_root_names(*e, expr_arena).pop().unwrap()).clone(),
                        alias,
                    },
                    _ => todo!(),
                }
            } else {
                unreachable!()
            }
        })
        .collect()
}

pub struct DefaultPlanner {}
impl Default for DefaultPlanner {
    fn default() -> Self {
        Self {}
    }
}

impl PhysicalPlanner for DefaultPlanner {
    fn create_physical_plan(
        &self,
        root: Node,
        lp_arena: &mut Arena<ALogicalPlan>,
        expr_arena: &mut Arena<AExpr>,
    ) -> Result<Box<dyn Executor>> {
        self.create_initial_physical_plan(root, lp_arena, expr_arena)
    }
}

impl DefaultPlanner {
    pub fn create_physical_expressions(
        &self,
        exprs: Vec<Node>,
        context: Context,
        expr_arena: &mut Arena<AExpr>,
    ) -> Result<Vec<Arc<dyn PhysicalExpr>>> {
        exprs
            .into_iter()
            .map(|e| self.create_physical_expr(e, context, expr_arena))
            .collect()
    }
    pub fn create_initial_physical_plan(
        &self,
        root: Node,
        lp_arena: &mut Arena<ALogicalPlan>,
        expr_arena: &mut Arena<AExpr>,
    ) -> Result<Box<dyn Executor>> {
        use ALogicalPlan::*;
        let logical_plan = lp_arena.take(root);
        match logical_plan {
            Melt {
                input,
                id_vars,
                value_vars,
                ..
            } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                Ok(Box::new(MeltExec {
                    input,
                    id_vars,
                    value_vars,
                }))
            }
            Slice { input, offset, len } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                Ok(Box::new(SliceExec { input, offset, len }))
            }
            Selection { input, predicate } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let predicate =
                    self.create_physical_expr(predicate, Context::Default, expr_arena)?;
                Ok(Box::new(FilterExec::new(predicate, input)))
            }
            CsvScan {
                path,
                schema,
                has_header,
                delimiter,
                ignore_errors,
                skip_rows,
                stop_after_n_rows,
                with_columns,
                predicate,
                aggregate,
                cache,
                low_memory,
            } => {
                let predicate = predicate
                    .map(|pred| self.create_physical_expr(pred, Context::Default, expr_arena))
                    .map_or(Ok(None), |v| v.map(Some))?;
                let aggregate = aggregate_expr_to_scan_agg(aggregate, expr_arena);
                Ok(Box::new(CsvExec {
                    path,
                    schema,
                    has_header,
                    delimiter,
                    ignore_errors,
                    skip_rows,
                    stop_after_n_rows,
                    with_columns,
                    predicate,
                    aggregate,
                    cache,
                    low_memory,
                }))
            }
            #[cfg(feature = "parquet")]
            ParquetScan {
                path,
                schema,
                with_columns,
                predicate,
                aggregate,
                stop_after_n_rows,
                cache,
            } => {
                let predicate = predicate
                    .map(|pred| self.create_physical_expr(pred, Context::Default, expr_arena))
                    .map_or(Ok(None), |v| v.map(Some))?;

                let aggregate = aggregate_expr_to_scan_agg(aggregate, expr_arena);
                Ok(Box::new(ParquetExec::new(
                    path,
                    schema,
                    with_columns,
                    predicate,
                    aggregate,
                    stop_after_n_rows,
                    cache,
                )))
            }
            Projection { expr, input, .. } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let phys_expr =
                    self.create_physical_expressions(expr, Context::Default, expr_arena)?;
                Ok(Box::new(StandardExec::new("projection", input, phys_expr)))
            }
            LocalProjection { expr, input, .. } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let phys_expr =
                    self.create_physical_expressions(expr, Context::Default, expr_arena)?;
                Ok(Box::new(StandardExec::new("projection", input, phys_expr)))
            }
            DataFrameScan {
                df,
                projection,
                selection,
                ..
            } => {
                let selection = selection
                    .map(|pred| self.create_physical_expr(pred, Context::Default, expr_arena))
                    .map_or(Ok(None), |v| v.map(Some))?;
                let projection = projection
                    .map(|proj| {
                        self.create_physical_expressions(proj, Context::Default, expr_arena)
                    })
                    .map_or(Ok(None), |v| v.map(Some))?;
                Ok(Box::new(DataFrameExec::new(df, projection, selection)))
            }
            Sort {
                input,
                by_column,
                reverse,
            } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let by_column =
                    self.create_physical_expressions(by_column, Context::Default, expr_arena)?;
                Ok(Box::new(SortExec {
                    input,
                    by_column,
                    reverse,
                }))
            }
            Explode { input, columns } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                Ok(Box::new(ExplodeExec { input, columns }))
            }
            Cache { input } => {
                let fields = lp_arena.get(input).schema(lp_arena).fields();
                // todo! fix the unique constraint in the schema. Probably in projection pushdown at joins
                let mut unique =
                    HashSet::with_capacity_and_hasher(fields.len(), RandomState::default());
                // assumption of 80 characters per column name
                let mut key = String::with_capacity(fields.len() * 80);
                for field in fields {
                    if unique.insert(field.name()) {
                        key.push_str(field.name())
                    }
                }
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                Ok(Box::new(CacheExec { key, input }))
            }
            Distinct {
                input,
                maintain_order,
                subset,
            } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let subset = Arc::try_unwrap(subset).unwrap_or_else(|subset| (*subset).clone());
                Ok(Box::new(DropDuplicatesExec {
                    input,
                    maintain_order,
                    subset,
                }))
            }
            Aggregate {
                input,
                keys,
                aggs,
                apply,
                ..
            } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;

                // We first check if we can partition the groupby on the latest moment.
                // TODO: fix this brittle/ buggy state and implement partitioned groupby's in eager
                let mut partitionable = true;

                if keys.len() == 1 {
                    for agg in &aggs {
                        // make sure that we don't have a binary expr in the expr tree
                        let matches =
                            |e: &AExpr| matches!(e, AExpr::SortBy { .. } | AExpr::Filter { .. });
                        if aexpr_to_root_nodes(*agg, expr_arena).len() != 1
                            || has_aexpr(*agg, expr_arena, matches)
                        {
                            partitionable = false;
                            break;
                        }

                        let agg = node_to_exp(*agg, expr_arena);

                        // check if the aggregation type is partitionable
                        match agg {
                            Expr::Agg(AggExpr::Min(_))
                            | Expr::Agg(AggExpr::Max(_))
                            | Expr::Agg(AggExpr::Sum(_))
                            | Expr::Agg(AggExpr::Mean(_))
                            // first need to implement this correctly
                            // | Expr::Agg(AggExpr::Count(_))
                            | Expr::Agg(AggExpr::Last(_))
                            | Expr::Agg(AggExpr::List(_))
                            | Expr::Agg(AggExpr::First(_)) => {}
                            _ => {
                                partitionable = false;
                                break
                            }
                        }
                    }
                } else {
                    partitionable = false;
                }
                // a custom function cannot be partitioned.
                if apply.is_some() {
                    partitionable = false;
                }
                let mut phys_keys =
                    self.create_physical_expressions(keys, Context::Default, expr_arena)?;
                if partitionable {
                    let phys_aggs = self.create_physical_expressions(
                        aggs.clone(),
                        Context::Aggregation,
                        expr_arena,
                    )?;
                    Ok(Box::new(PartitionGroupByExec::new(
                        input,
                        phys_keys.pop().unwrap(),
                        phys_aggs,
                        aggs.into_iter()
                            .map(|n| node_to_exp(n, expr_arena))
                            .collect(),
                    )))
                } else {
                    let phys_aggs =
                        self.create_physical_expressions(aggs, Context::Aggregation, expr_arena)?;
                    Ok(Box::new(GroupByExec::new(
                        input, phys_keys, phys_aggs, apply,
                    )))
                }
            }
            Join {
                input_left,
                input_right,
                how,
                left_on,
                right_on,
                allow_par,
                force_par,
                ..
            } => {
                let parallel = if force_par {
                    force_par
                } else if allow_par {
                    // check if two DataFrames come from a separate source.
                    // If they don't we can parallelize,
                    // Otherwise it is in cache.
                    let mut sources_left =
                        HashSet::with_capacity_and_hasher(16, RandomState::default());
                    agg_source_paths(input_left, &mut sources_left, lp_arena);
                    let mut sources_right =
                        HashSet::with_capacity_and_hasher(16, RandomState::default());
                    agg_source_paths(input_right, &mut sources_right, lp_arena);
                    sources_left
                        .intersection(&sources_right)
                        .collect_vec()
                        .is_empty()
                } else {
                    false
                };

                let input_left =
                    self.create_initial_physical_plan(input_left, lp_arena, expr_arena)?;
                let input_right =
                    self.create_initial_physical_plan(input_right, lp_arena, expr_arena)?;
                let left_on =
                    self.create_physical_expressions(left_on, Context::Default, expr_arena)?;
                let right_on =
                    self.create_physical_expressions(right_on, Context::Default, expr_arena)?;
                Ok(Box::new(JoinExec::new(
                    input_left,
                    input_right,
                    how,
                    left_on,
                    right_on,
                    parallel,
                )))
            }
            HStack { input, exprs, .. } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                let phys_expr =
                    self.create_physical_expressions(exprs, Context::Default, expr_arena)?;
                Ok(Box::new(StackExec::new(input, phys_expr)))
            }
            Udf {
                input, function, ..
            } => {
                let input = self.create_initial_physical_plan(input, lp_arena, expr_arena)?;
                Ok(Box::new(UdfExec { input, function }))
            }
        }
    }

    pub fn create_physical_expr(
        &self,
        expression: Node,
        ctxt: Context,
        expr_arena: &mut Arena<AExpr>,
    ) -> Result<Arc<dyn PhysicalExpr>> {
        use AExpr::*;

        match expr_arena.get(expression).clone() {
            Window {
                mut function,
                partition_by,
                order_by: _,
            } => {
                // TODO! Order by
                let group_column =
                    self.create_physical_expr(partition_by, Context::Default, expr_arena)?;
                let mut out_name = None;
                let mut apply_columns = aexpr_to_root_names(function, expr_arena);
                if apply_columns.len() > 1 {
                    return Err(PolarsError::ValueError(
                        "Binary/Ternary function not yet supported in window expressions".into(),
                    ));
                }
                let apply_column = apply_columns.pop().unwrap();

                if let Alias(expr, name) = expr_arena.get(function) {
                    function = *expr;
                    out_name = Some(name.clone());
                };
                let function = node_to_exp(function, expr_arena);

                Ok(Arc::new(WindowExpr {
                    group_column,
                    apply_column,
                    out_name,
                    function,
                }))
            }
            Literal(value) => Ok(Arc::new(LiteralExpr::new(
                value,
                node_to_exp(expression, expr_arena),
            ))),
            BinaryExpr { left, op, right } => {
                let lhs = self.create_physical_expr(left, ctxt, expr_arena)?;
                let rhs = self.create_physical_expr(right, ctxt, expr_arena)?;
                Ok(Arc::new(phys_expr::binary::BinaryExpr::new(
                    lhs,
                    op,
                    rhs,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Column(column) => Ok(Arc::new(ColumnExpr::new(
                column,
                node_to_exp(expression, expr_arena),
            ))),
            Sort { expr, reverse } => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(SortExpr::new(
                    phys_expr,
                    reverse,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Take { expr, idx } => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let phys_idx = self.create_physical_expr(idx, ctxt, expr_arena)?;
                Ok(Arc::new(TakeExpr::new(phys_expr, phys_idx)))
            }
            SortBy { expr, by, reverse } => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let phys_by = self.create_physical_expr(by, ctxt, expr_arena)?;
                Ok(Arc::new(SortByExpr::new(
                    phys_expr,
                    phys_by,
                    reverse,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Filter { input, by } => {
                let phys_input = self.create_physical_expr(input, ctxt, expr_arena)?;
                let phys_by = self.create_physical_expr(by, ctxt, expr_arena)?;
                Ok(Arc::new(FilterExpr::new(
                    phys_input,
                    phys_by,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Not(expr) => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(NotExpr::new(
                    phys_expr,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Alias(expr, name) => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(AliasExpr::new(
                    phys_expr,
                    name,
                    node_to_exp(expression, expr_arena),
                )))
            }
            IsNull(expr) => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(IsNullExpr::new(
                    phys_expr,
                    node_to_exp(expression, expr_arena),
                )))
            }
            IsNotNull(expr) => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(IsNotNullExpr::new(
                    phys_expr,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Agg(agg) => {
                match agg {
                    AAggExpr::Min(expr) => {
                        // todo! Output type is dependent on schema.
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Min)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    parallel_op(|s| Ok(s.min_as_series()), s, None)
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Max(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Max)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    parallel_op(|s| Ok(s.max_as_series()), s, None)
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Sum(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Sum)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    parallel_op(|s| Ok(s.sum_as_series()), s, None)
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Std(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Std)))
                            }
                            Context::Default => {
                                let function =
                                    NoEq::new(Arc::new(move |s: Series| Ok(s.std_as_series()))
                                        as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Var(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Var)))
                            }
                            Context::Default => {
                                let function =
                                    NoEq::new(Arc::new(move |s: Series| Ok(s.var_as_series()))
                                        as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Mean(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Mean)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    let len = s.len() as f64;
                                    parallel_op(|s| Ok(s.sum_as_series()), s, None)
                                        .map(|s| s.cast::<Float64Type>().unwrap() / len)
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Median(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Median)))
                            }
                            Context::Default => {
                                let function =
                                    NoEq::new(Arc::new(move |s: Series| Ok(s.median_as_series()))
                                        as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::First(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::First)))
                            }
                            Context::Default => {
                                let function =
                                    NoEq::new(Arc::new(move |s: Series| Ok(s.head(Some(1))))
                                        as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Last(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Last)))
                            }
                            Context::Default => {
                                let function =
                                    NoEq::new(Arc::new(move |s: Series| Ok(s.tail(Some(1))))
                                        as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::List(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::List)))
                            }
                            Context::Default => {
                                panic!(
                                    "list expression is only supported in the aggregation context"
                                )
                            }
                        }
                    }
                    AAggExpr::NUnique(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => Ok(Arc::new(AggregationExpr::new(
                                input,
                                GroupByMethod::NUnique,
                            ))),
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    s.n_unique().map(|count| {
                                        UInt32Chunked::new_from_slice(s.name(), &[count as u32])
                                            .into_series()
                                    })
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: Some(DataType::UInt32),
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::Quantile { expr, quantile } => {
                        // todo! add schema to get correct output type
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggQuantileExpr::new(input, quantile)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    s.quantile_as_series(quantile)
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: None,
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                    AAggExpr::AggGroups(expr) => {
                        if let Context::Default = ctxt {
                            panic!("agg groups expression only supported in aggregation context")
                        }
                        let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        Ok(Arc::new(AggregationExpr::new(
                            phys_expr,
                            GroupByMethod::Groups,
                        )))
                    }
                    AAggExpr::Count(expr) => {
                        let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                        match ctxt {
                            Context::Aggregation => {
                                Ok(Arc::new(AggregationExpr::new(input, GroupByMethod::Count)))
                            }
                            Context::Default => {
                                let function = NoEq::new(Arc::new(move |s: Series| {
                                    let count = s.len();
                                    Ok(UInt32Chunked::new_from_slice(s.name(), &[count as u32])
                                        .into_series())
                                })
                                    as Arc<dyn SeriesUdf>);
                                Ok(Arc::new(ApplyExpr {
                                    input,
                                    function,
                                    output_type: Some(DataType::UInt32),
                                    expr: node_to_exp(expression, expr_arena),
                                }))
                            }
                        }
                    }
                }
            }
            Cast { expr, data_type } => {
                let phys_expr = self.create_physical_expr(expr, ctxt, expr_arena)?;
                Ok(Arc::new(CastExpr::new(phys_expr, data_type)))
            }
            Ternary {
                predicate,
                truthy,
                falsy,
            } => {
                let predicate = self.create_physical_expr(predicate, ctxt, expr_arena)?;
                let truthy = self.create_physical_expr(truthy, ctxt, expr_arena)?;
                let falsy = self.create_physical_expr(falsy, ctxt, expr_arena)?;
                Ok(Arc::new(TernaryExpr {
                    predicate,
                    truthy,
                    falsy,
                    expr: node_to_exp(expression, expr_arena),
                }))
            }
            Udf {
                input,
                function,
                output_type,
            } => {
                let input = self.create_physical_expr(input, ctxt, expr_arena)?;
                Ok(Arc::new(ApplyExpr {
                    input,
                    function,
                    output_type,
                    expr: node_to_exp(expression, expr_arena),
                }))
            }
            BinaryFunction {
                input_a,
                input_b,
                function,
                output_field,
            } => {
                let input_a = self.create_physical_expr(input_a, ctxt, expr_arena)?;
                let input_b = self.create_physical_expr(input_b, ctxt, expr_arena)?;
                Ok(Arc::new(BinaryFunctionExpr {
                    input_a,
                    input_b,
                    function,
                    output_field,
                }))
            }
            Shift { input, periods } => {
                let input = self.create_physical_expr(input, ctxt, expr_arena)?;
                let function = NoEq::new(
                    Arc::new(move |s: Series| Ok(s.shift(periods))) as Arc<dyn SeriesUdf>
                );
                Ok(Arc::new(ApplyExpr::new(
                    input,
                    function,
                    None,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Slice {
                input,
                offset,
                length,
            } => {
                let input = self.create_physical_expr(input, ctxt, expr_arena)?;
                Ok(Arc::new(SliceExpr {
                    input,
                    offset,
                    len: length,
                }))
            }
            Reverse(expr) => {
                let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let function =
                    NoEq::new(Arc::new(move |s: Series| Ok(s.reverse())) as Arc<dyn SeriesUdf>);
                Ok(Arc::new(ApplyExpr::new(
                    input,
                    function,
                    None,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Duplicated(expr) => {
                let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let function = NoEq::new(Arc::new(move |s: Series| {
                    s.is_duplicated().map(|ca| ca.into_series())
                }) as Arc<dyn SeriesUdf>);
                Ok(Arc::new(ApplyExpr::new(
                    input,
                    function,
                    None,
                    node_to_exp(expression, expr_arena),
                )))
            }
            IsUnique(expr) => {
                let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let function =
                    NoEq::new(
                        Arc::new(move |s: Series| s.is_unique().map(|ca| ca.into_series()))
                            as Arc<dyn SeriesUdf>,
                    );
                Ok(Arc::new(ApplyExpr::new(
                    input,
                    function,
                    None,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Explode(expr) => {
                let input = self.create_physical_expr(expr, ctxt, expr_arena)?;
                let function =
                    NoEq::new(Arc::new(move |s: Series| s.explode()) as Arc<dyn SeriesUdf>);
                Ok(Arc::new(ApplyExpr::new(
                    input,
                    function,
                    None,
                    node_to_exp(expression, expr_arena),
                )))
            }
            Wildcard => panic!("should be no wildcard at this point"),
            Except(_) => panic!("should be no except at this point"),
        }
    }
}
