// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
use super::FirebirdDialect;
use crate::sql_schema_calculator::{Context, SqlSchemaCalculatorFlavour};
use sql_schema_describer::firebird::{FirebirdSchemaExt, IndexMetadata};

impl SqlSchemaCalculatorFlavour for FirebirdDialect {
    fn datamodel_connector(&self) -> &dyn psl::datamodel_connector::Connector {
        psl::builtin_connectors::FIREBIRD
    }

    fn push_connector_data(&self, context: &mut Context<'_>) {
        let mut ext = FirebirdSchemaExt::default();
        for table in context.schema.describer_schema.table_walkers() {
            for index in table.indexes() {
                ext.indexes.push((
                    index.id,
                    IndexMetadata {
                        database_name: index.name().to_owned(),
                        constraint_name: (index.is_unique() || index.is_primary_key()).then(|| index.name().to_owned()),
                    },
                ));
            }
        }
        context.schema.describer_schema.set_connector_data(Box::new(ext));
    }
}
