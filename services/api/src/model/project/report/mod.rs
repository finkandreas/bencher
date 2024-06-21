use bencher_json::{
    project::report::{Adapter, Iteration, JsonReportAlerts, JsonReportResult, JsonReportResults},
    DateTime, JsonNewReport, JsonReport, ReportUuid,
};
use diesel::{
    ExpressionMethods, NullableExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper,
};
use dropshot::HttpError;
use slog::Logger;

use crate::{
    conn_lock,
    context::{ApiContext, DbConnection},
    error::resource_not_found_err,
    model::{
        project::{
            benchmark::QueryBenchmark,
            branch::BranchId,
            measure::QueryMeasure,
            testbed::{QueryTestbed, TestbedId},
            threshold::{alert::QueryAlert, model::QueryModel, QueryThreshold},
            version::VersionId,
            ProjectId, QueryProject,
        },
        user::{QueryUser, UserId},
    },
    schema::{self, report as report_table},
    util::fn_get::{fn_get_id, fn_get_uuid},
    view,
};

use super::{
    branch_version::QueryBranchVersion, metric::QueryMetric, metric_boundary::QueryMetricBoundary,
    threshold::boundary::QueryBoundary,
};

pub mod report_benchmark;
pub mod results;

crate::util::typed_id::typed_id!(ReportId);

#[derive(diesel::Queryable, diesel::Identifiable, diesel::Associations, diesel::Selectable)]
#[diesel(table_name = report_table)]
#[diesel(belongs_to(QueryProject, foreign_key = project_id))]
pub struct QueryReport {
    pub id: ReportId,
    pub uuid: ReportUuid,
    pub user_id: UserId,
    pub project_id: ProjectId,
    pub branch_id: BranchId,
    pub version_id: VersionId,
    pub testbed_id: TestbedId,
    pub adapter: Adapter,
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub created: DateTime,
}

impl QueryReport {
    fn_get_id!(report, ReportId, ReportUuid);
    fn_get_uuid!(report, ReportId, ReportUuid);

    pub async fn into_json(
        self,
        log: &Logger,
        context: &ApiContext,
    ) -> Result<JsonReport, HttpError> {
        let Self {
            id,
            uuid,
            user_id,
            project_id,
            branch_id,
            version_id,
            testbed_id,
            adapter,
            start_time,
            end_time,
            created,
        } = self;

        let query_project = QueryProject::get(conn_lock!(context), project_id)?;
        let user = QueryUser::get(conn_lock!(context), user_id)?.into_json();
        let branch = QueryBranchVersion::get_json_for_project(
            context,
            &query_project,
            branch_id,
            version_id,
        )
        .await?;
        let testbed = QueryTestbed::get(conn_lock!(context), testbed_id)?
            .into_json_for_project(&query_project);
        let results = get_report_results(log, conn_lock!(context), &query_project, id)?;
        let alerts = get_report_alerts(conn_lock!(context), &query_project, id)?;

        let project = query_project.into_json(conn_lock!(context))?;
        Ok(JsonReport {
            uuid,
            user,
            project,
            branch,
            testbed,
            start_time,
            end_time,
            adapter,
            results,
            alerts,
            created,
        })
    }
}

type ResultsQuery = (
    Iteration,
    QueryBenchmark,
    QueryMeasure,
    QueryMetricBoundary,
    Option<(QueryThreshold, QueryModel)>,
);

fn get_report_results(
    log: &Logger,
    conn: &mut DbConnection,
    project: &QueryProject,
    report_id: ReportId,
) -> Result<JsonReportResults, HttpError> {
    schema::report_benchmark::table
    .filter(schema::report_benchmark::report_id.eq(report_id))
    .inner_join(schema::benchmark::table)
    .inner_join(view::metric_boundary::table
        .inner_join(schema::measure::table)
        // There may or may not be a boundary for any given metric
        .left_join(schema::threshold::table)
        .left_join(schema::model::table)
    )
    // It is important to order by the iteration first in order to make sure they are grouped together below
    // Then ordering by measure and finally benchmark name makes sure that the benchmarks are in the same order for each iteration
    .order((schema::report_benchmark::iteration, schema::measure::name, schema::benchmark::name))
    .select((
        schema::report_benchmark::iteration,
        QueryBenchmark::as_select(),
        QueryMeasure::as_select(),
        QueryMetricBoundary::as_select(),
        (
            (
                schema::threshold::id,
                schema::threshold::uuid,
                schema::threshold::project_id,
                schema::threshold::measure_id,
                schema::threshold::branch_id,
                schema::threshold::testbed_id,
                schema::threshold::model_id,
                schema::threshold::created,
                schema::threshold::modified,
            ),
            (
                schema::model::id,
                schema::model::uuid,
                schema::model::threshold_id,
                schema::model::test,
                schema::model::min_sample_size,
                schema::model::max_sample_size,
                schema::model::window,
                schema::model::lower_boundary,
                schema::model::upper_boundary,
                schema::model::created,
                schema::model::replaced,
            )
        ).nullable(),
    ))
    .load::<ResultsQuery>(conn)
    .map(|results| into_report_results_json(log, project, results))
    .map_err(resource_not_found_err!(ReportBenchmark, project))
}

fn into_report_results_json(
    log: &Logger,
    project: &QueryProject,
    results: Vec<ResultsQuery>,
) -> JsonReportResults {
    let mut report_results = Vec::new();
    let mut report_iteration = Vec::new();
    let mut prev_iteration = None;
    let mut report_result: Option<JsonReportResult> = None;
    for (iteration, query_benchmark, query_measure, query_metric_boundary, threshold_model) in
        results
    {
        // If onto a new iteration, then add the result to the report iteration list.
        // Then add the report iteration list to the report results list.
        if let Some(prev_iteration) = prev_iteration.take() {
            if iteration != prev_iteration {
                slog::trace!(log, "Iteration {prev_iteration} => {iteration}");
                if let Some(result) = report_result.take() {
                    report_iteration.push(result);
                }
                if !report_iteration.is_empty() {
                    report_results.push(std::mem::take(&mut report_iteration));
                }
            }
        }
        prev_iteration = Some(iteration);

        // If there is a current report result, make sure that the measure is the same.
        // Otherwise, add it to the report iteration list.
        if let Some(result) = report_result.take() {
            if query_measure.uuid == result.measure.uuid {
                report_result = Some(result);
            } else {
                slog::trace!(
                    log,
                    "Measure {} => {}",
                    result.measure.uuid,
                    query_measure.uuid,
                );
                report_iteration.push(result);
            }
        }

        let (query_metric, query_boundary) = query_metric_boundary.split();
        // Create a benchmark metric out of the benchmark, metric, and boundary
        let benchmark_metric =
            query_benchmark.into_benchmark_metric_json(project, query_metric, query_boundary);

        // If there is a current report result, add the benchmark metric to it.
        // Otherwise, create a new report result and add the benchmark to it.
        if let Some(result) = report_result.as_mut() {
            result.benchmarks.push(benchmark_metric);
        } else {
            report_result = Some(JsonReportResult {
                iteration,
                measure: query_measure.into_json_for_project(project),
                threshold: threshold_model.map(|(threshold, model)| {
                    threshold.into_threshold_model_json_for_project(project, model)
                }),
                benchmarks: vec![benchmark_metric],
            });
        }
    }

    // Save from the last iteration
    if let Some(result) = report_result.take() {
        report_iteration.push(result);
    }
    if !report_iteration.is_empty() {
        report_results.push(report_iteration);
    }
    slog::trace!(log, "Report results: {report_results:#?}");

    report_results
}

fn get_report_alerts(
    conn: &mut DbConnection,
    project: &QueryProject,
    report_id: ReportId,
) -> Result<JsonReportAlerts, HttpError> {
    let alerts = schema::alert::table
        .inner_join(
            schema::boundary::table.inner_join(
                schema::metric::table.inner_join(
                    schema::report_benchmark::table
                        .inner_join(schema::report::table)
                        .inner_join(schema::benchmark::table),
                ),
            ),
        )
        .filter(schema::report::id.eq(report_id))
        .order((schema::report_benchmark::iteration, schema::benchmark::name))
        .select((
            schema::report::uuid,
            schema::report::created,
            schema::report_benchmark::iteration,
            QueryAlert::as_select(),
            QueryBenchmark::as_select(),
            QueryMetric::as_select(),
            QueryBoundary::as_select(),
        ))
        .load::<(
            ReportUuid,
            DateTime,
            Iteration,
            QueryAlert,
            QueryBenchmark,
            QueryMetric,
            QueryBoundary,
        )>(conn)
        .map_err(resource_not_found_err!(Alert, report_id))?;

    let mut report_alerts = Vec::new();
    for (
        report_uuid,
        created,
        iteration,
        query_alert,
        query_benchmark,
        query_metric,
        query_boundary,
    ) in alerts
    {
        let json_alert = query_alert.into_json_for_report(
            conn,
            project,
            report_uuid,
            created,
            iteration,
            query_benchmark,
            query_metric,
            query_boundary,
        )?;
        report_alerts.push(json_alert);
    }

    Ok(report_alerts)
}

#[derive(Debug, diesel::Insertable)]
#[diesel(table_name = report_table)]
pub struct InsertReport {
    pub uuid: ReportUuid,
    pub user_id: UserId,
    pub project_id: ProjectId,
    pub branch_id: BranchId,
    pub version_id: VersionId,
    pub testbed_id: TestbedId,
    pub adapter: Adapter,
    pub start_time: DateTime,
    pub end_time: DateTime,
    pub created: DateTime,
}

impl InsertReport {
    pub fn from_json(
        user_id: UserId,
        project_id: ProjectId,
        branch_id: BranchId,
        version_id: VersionId,
        testbed_id: TestbedId,
        report: &JsonNewReport,
        adapter: Adapter,
    ) -> Self {
        Self {
            uuid: ReportUuid::new(),
            user_id,
            project_id,
            branch_id,
            version_id,
            testbed_id,
            adapter,
            start_time: report.start_time,
            end_time: report.end_time,
            created: DateTime::now(),
        }
    }
}
