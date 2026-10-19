// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
use super::{
    FirebirdDialect,
    renderer::{default_sql, native_type_sql},
};
use crate::{
    migration_pair::MigrationPair,
    sql_migration::SqlMigrationStep,
    sql_schema_differ::{ColumnChanges, SqlSchemaDifferFlavour, column::ColumnTypeChange, table::TableDiffer},
};
use psl::builtin_connectors::FirebirdType;
use sql_schema_describer::{
    DefaultKind, PrismaValue, TableColumnId,
    firebird::FirebirdSchemaExt,
    walkers::{IndexWalker, TableColumnWalker},
};

impl SqlSchemaDifferFlavour for FirebirdDialect {
    fn column_defaults_match(&self, columns: MigrationPair<TableColumnWalker<'_>>) -> Option<bool> {
        let temporal_default = |column: TableColumnWalker<'_>| {
            matches!(
                column.default().map(|default| default.kind()),
                Some(DefaultKind::Value(PrismaValue::DateTime(_)) | DefaultKind::Now)
            )
        };
        if !temporal_default(columns.previous) && !temporal_default(columns.next) {
            return None;
        }
        Some(match (default_sql(columns.previous), default_sql(columns.next)) {
            (Ok(Some(previous)), Ok(Some(next))) => previous.eq_ignore_ascii_case(&next),
            (Ok(None), Ok(None)) => true,
            // Force a real step so preflight reports the unsupported default.
            _ => false,
        })
    }

    fn push_index_changes_for_column_changes(
        &self,
        table: &TableDiffer<'_, '_>,
        columns: MigrationPair<TableColumnId>,
        changes: ColumnChanges,
        steps: &mut Vec<SqlMigrationStep>,
    ) {
        if !changes.type_changed() {
            return;
        }
        // Firebird refuses ALTER TYPE while an index participates in an integrity
        // constraint. Schedule the drop before AlterTable, and recreation after it.
        for indexes in table
            .index_pairs()
            .filter(|indexes| indexes.previous.contains_column(columns.previous))
        {
            steps.retain(
                |step| !matches!(step, SqlMigrationStep::RedefineIndex { index } if *index == indexes.map(|i| i.id)),
            );
            if !steps.iter().any(
                |step| matches!(step, SqlMigrationStep::DropIndex { index_id } if *index_id == indexes.previous.id),
            ) {
                steps.push(SqlMigrationStep::DropIndex {
                    index_id: indexes.previous.id,
                });
            }
            if !steps.iter().any(
                |step| matches!(step, SqlMigrationStep::CreateIndex { index_id, .. } if *index_id == indexes.next.id),
            ) {
                steps.push(SqlMigrationStep::CreateIndex {
                    table_id: (Some(table.previous().id), table.next().id),
                    index_id: indexes.next.id,
                    from_drop_and_recreate: true,
                });
            }
        }
    }
    fn can_rename_foreign_key(&self) -> bool {
        false
    }
    fn can_rename_index(&self) -> bool {
        false
    }
    fn table_should_be_ignored(&self, table: &str) -> bool {
        table == "_prisma_migrate_lock"
    }

    fn indexes_match(&self, previous: IndexWalker<'_>, next: IndexWalker<'_>) -> bool {
        // A standalone UNIQUE INDEX cannot be the target of a Firebird foreign key.
        // Do not silently treat it as a UNIQUE constraint generated from @@unique.
        let is_constraint = |index: IndexWalker<'_>| {
            let ext: &FirebirdSchemaExt = index.schema.downcast_connector_data();
            ext.index_metadata(index.id)
                .and_then(|m| m.constraint_name.as_ref())
                .is_some()
        };
        is_constraint(previous) == is_constraint(next)
    }

    fn column_type_change(&self, columns: MigrationPair<TableColumnWalker<'_>>) -> Option<ColumnTypeChange> {
        let previous = columns.previous.column_native_type::<FirebirdType>();
        let next = columns.next.column_native_type::<FirebirdType>();
        match (previous, next) {
            (Some(previous), Some(next)) => {
                if let (Ok(previous), Ok(next)) = (native_type_sql(previous), native_type_sql(next)) {
                    if previous == next {
                        return None;
                    }
                }
                use FirebirdType::*;
                let widening = match (previous, next) {
                    (SmallInt, Integer | BigInt) | (Integer, BigInt) | (Float, DoublePrecision) => true,
                    (Char(a), Char(b)) | (VarChar(a), VarChar(b)) => a < b,
                    (Decimal(a), Decimal(b)) | (Numeric(a), Numeric(b)) => {
                        let (ap, ascale) = a.unwrap_or((9, 0));
                        let (bp, bscale) = b.unwrap_or((9, 0));
                        ap < bp && ascale == bscale
                    }
                    _ => false,
                };
                Some(if widening {
                    ColumnTypeChange::SafeCast
                } else {
                    ColumnTypeChange::NotCastable
                })
            }
            (None, None)
                if columns.previous.column_type_family() == columns.next.column_type_family()
                    && columns.previous.column_type().full_data_type == columns.next.column_type().full_data_type =>
            {
                None
            }
            _ => Some(ColumnTypeChange::NotCastable),
        }
    }
}
