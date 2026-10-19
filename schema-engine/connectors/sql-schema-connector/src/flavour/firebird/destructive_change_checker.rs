// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
use super::{FirebirdDialect, renderer::quote_identifier};
use crate::{
    flavour::SqlConnector,
    migration_pair::MigrationPair,
    sql_destructive_change_checker::{
        DestructiveChangeCheckerFlavour,
        check::{Column, Table},
        destructive_change_checker_flavour::{extract_column_values_count, extract_table_rows_count},
        destructive_check_plan::DestructiveCheckPlan,
        unexecutable_step_check::UnexecutableStepCheck,
        warning_check::SqlMigrationWarningCheck,
    },
    sql_migration::{AlterColumn, ColumnTypeChange},
    sql_schema_differ::ColumnChanges,
};
use schema_connector::{BoxFuture, ConnectorResult};
use sql_schema_describer::walkers::TableColumnWalker;

impl DestructiveChangeCheckerFlavour for FirebirdDialect {
    fn check_alter_column(
        &self,
        alter: &AlterColumn,
        columns: &MigrationPair<TableColumnWalker<'_>>,
        plan: &mut DestructiveCheckPlan,
        step: usize,
    ) {
        if columns.previous.arity().is_nullable() && columns.next.arity().is_required() {
            // A new DEFAULT does not replace NULLs already stored in Firebird.
            plan.push_unexecutable(
                UnexecutableStepCheck::MadeOptionalFieldRequired(Column {
                    table: columns.previous.table().name().to_owned(),
                    namespace: None,
                    column: columns.previous.name().to_owned(),
                }),
                step,
            );
        }
        if matches!(alter.type_change, Some(ColumnTypeChange::RiskyCast)) {
            plan.push_warning(
                SqlMigrationWarningCheck::RiskyCast {
                    table: columns.previous.table().name().to_owned(),
                    namespace: None,
                    column: columns.previous.name().to_owned(),
                    previous_type: format!("{:?}", columns.previous.column_type_family()),
                    next_type: format!("{:?}", columns.next.column_type_family()),
                },
                step,
            );
        }
    }

    fn check_drop_and_recreate_column(
        &self,
        columns: &MigrationPair<TableColumnWalker<'_>>,
        _changes: &ColumnChanges,
        plan: &mut DestructiveCheckPlan,
        step: usize,
    ) {
        plan.push_warning(
            SqlMigrationWarningCheck::DropAndRecreateColumn {
                table: columns.previous.table().name().to_owned(),
                namespace: None,
                column: columns.previous.name().to_owned(),
            },
            step,
        );
    }

    fn count_rows_in_table<'a>(
        &'a mut self,
        connector: &'a mut dyn SqlConnector,
        table: &'a Table,
    ) -> BoxFuture<'a, ConnectorResult<i64>> {
        Box::pin(async move {
            let result = connector
                .query_raw(&format!("SELECT COUNT(*) FROM {}", quote_identifier(&table.table)), &[])
                .await?;
            extract_table_rows_count(table, result)
        })
    }

    fn count_values_in_column<'a>(
        &'a mut self,
        connector: &'a mut dyn SqlConnector,
        column: &'a Column,
    ) -> BoxFuture<'a, ConnectorResult<i64>> {
        Box::pin(async move {
            let query = format!(
                "SELECT COUNT(*) FROM {} WHERE {} IS NOT NULL",
                quote_identifier(&column.table),
                quote_identifier(&column.column)
            );
            extract_column_values_count(connector.query_raw(&query, &[]).await?)
        })
    }
}
