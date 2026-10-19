#![cfg(feature = "firebird")]
// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

use psl::ValidatedSchema;

const SOURCE: &str = r#"
datasource db {
  provider = "firebird"
}
"#;

#[derive(Clone, Copy, Debug)]
enum ConstraintKind {
    PrimaryKey,
    Unique,
    Index,
    ForeignKey,
}

const CONSTRAINT_KINDS: [ConstraintKind; 4] = [
    ConstraintKind::PrimaryKey,
    ConstraintKind::Unique,
    ConstraintKind::Index,
    ConstraintKind::ForeignKey,
];

fn model_with_constraint(model: &str, kind: ConstraintKind, name: &str) -> String {
    let primary_key = match kind {
        ConstraintKind::PrimaryKey => format!("@id(map: \"{name}\")"),
        _ => "@id".to_owned(),
    };
    let unique = match kind {
        ConstraintKind::Unique => format!("@unique(map: \"{name}\")"),
        _ => String::new(),
    };
    let foreign_key = match kind {
        ConstraintKind::ForeignKey => name.to_owned(),
        _ => format!("{model}_target_fk"),
    };
    let index = match kind {
        ConstraintKind::Index => format!("@@index([value], map: \"{name}\")"),
        _ => String::new(),
    };

    format!(
        r#"
model {model} {{
  id Int {primary_key}
  value String {unique}
  targetId Int
  target Target @relation(fields: [targetId], references: [id], map: "{foreign_key}")

  {index}
}}
"#
    )
}

fn validate(models: &str) -> ValidatedSchema {
    psl::validate_without_extensions(format!("{SOURCE}\n{models}").into())
}

#[test]
fn named_constraints_share_one_global_namespace_across_models_and_kinds() {
    let target = "model Target {\n  id Int @id\n  first First[]\n  second Second[]\n}\n";

    for (first_index, first_kind) in CONSTRAINT_KINDS.iter().enumerate() {
        for second_kind in &CONSTRAINT_KINDS[first_index..] {
            let distinct = validate(&format!(
                "{target}{}{}",
                model_with_constraint("First", *first_kind, "first_named"),
                model_with_constraint("Second", *second_kind, "second_named"),
            ));
            assert!(
                !distinct.diagnostics.has_errors(),
                "Distinct {first_kind:?}/{second_kind:?} constraint names must validate:\n{}",
                distinct.render_own_diagnostics()
            );

            let duplicate = validate(&format!(
                "{target}{}{}",
                model_with_constraint("First", *first_kind, "collision"),
                model_with_constraint("Second", *second_kind, "collision"),
            ));
            let errors = duplicate.diagnostics.errors();
            assert_eq!(
                errors.len(),
                2,
                "Both {first_kind:?}/{second_kind:?} constraints must report the collision:\n{}",
                duplicate.render_own_diagnostics()
            );
            for error in errors {
                assert!(error.message().contains("collision"), "{}", error.message());
                assert!(error.message().contains("has to be unique"), "{}", error.message());
                assert!(
                    error
                        .message()
                        .contains("global for primary key, foreign keys, indexes and unique constraints"),
                    "{}",
                    error.message()
                );
            }
        }
    }
}

#[test]
fn explicit_constraint_names_observe_the_31_byte_limit_for_every_constraint_kind() {
    let target = "model Target {\n  id Int @id\n  first First[]\n}\n";

    for kind in CONSTRAINT_KINDS {
        for name in ["c".repeat(31), format!("{}x", "\u{00e9}".repeat(15))] {
            assert_eq!(name.len(), 31);
            let schema = validate(&format!("{target}{}", model_with_constraint("First", kind, &name)));
            assert!(
                !schema.diagnostics.has_errors(),
                "A 31-byte {kind:?} name must validate:\n{}",
                schema.render_own_diagnostics()
            );
        }

        for name in ["c".repeat(32), "\u{00e9}".repeat(16)] {
            assert_eq!(name.len(), 32);
            let schema = validate(&format!("{target}{}", model_with_constraint("First", kind, &name)));
            let errors = schema.diagnostics.errors();
            assert_eq!(
                errors.len(),
                1,
                "A 32-byte {kind:?} name must be rejected:\n{}",
                schema.render_own_diagnostics()
            );
            assert!(errors[0].message().contains("too long"), "{}", errors[0].message());
            assert!(errors[0].message().contains("31 bytes"), "{}", errors[0].message());
        }
    }
}
