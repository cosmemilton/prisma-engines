// Modified for experimental Firebird support (2026-10-20); see MODIFICATIONS.md.
mod create;
mod update;

pub(crate) use create::*;
pub(crate) use update::*;

use super::*;
use query_structure::prelude::*;

fn current_date_input_type<'a>() -> InputType<'a> {
    let mut object = init_input_object_type(Identifier::new_prisma("FirebirdCurrentDateInput"));
    object.require_exactly_one_field();
    object.set_fields(|| {
        vec![simple_input_field(
            constants::operations::CURRENT_DATE,
            InputType::boolean(),
            None,
        )]
    });
    InputType::object(object)
}

// Todo: This isn't final, this is only the first draft to get structure into the
// wild cross-dependency waste that was the create/update inputs.
pub(crate) trait DataInputFieldMapper {
    fn map_all<'a>(&self, ctx: &'a QuerySchema, fields: impl Iterator<Item = Field>) -> Vec<InputField<'a>> {
        fields
            .into_iter()
            .map(|field| match field {
                Field::Scalar(sf) if sf.is_list() => self.map_scalar_list(ctx, sf),
                Field::Scalar(sf) => self.map_scalar(ctx, sf),
                Field::Relation(rf) => self.map_relation(ctx, rf),
                Field::Composite(cf) => self.map_composite(ctx, cf),
            })
            .collect()
    }

    fn map_scalar<'a>(&self, ctx: &'a QuerySchema, sf: ScalarFieldRef) -> InputField<'a>;

    fn map_scalar_list<'a>(&self, ctx: &'a QuerySchema, sf: ScalarFieldRef) -> InputField<'a>;

    fn map_relation<'a>(&self, ctx: &'a QuerySchema, rf: RelationFieldRef) -> InputField<'a>;

    fn map_composite<'a>(&self, ctx: &'a QuerySchema, cf: CompositeFieldRef) -> InputField<'a>;
}
