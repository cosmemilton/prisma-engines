// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
crate::native_type_definition! {
    /// Native types for the initial Firebird 3.0 schema connector.
    FirebirdType;
    SmallInt -> Int,
    Integer -> Int,
    BigInt -> BigInt,
    Float -> Float,
    DoublePrecision -> Float,
    /// An omitted precision and scale uses Firebird's native default (9, 0).
    Decimal(Option<(u32, u32)>) -> Decimal,
    /// An omitted precision and scale uses Firebird's native default (9, 0).
    Numeric(Option<(u32, u32)>) -> Decimal,
    Boolean -> Boolean,
    Char(u32) -> String,
    VarChar(u32) -> String,
    /// BLOB SUB_TYPE TEXT.
    Text -> String,
    /// BLOB SUB_TYPE BINARY.
    Blob -> Bytes,
    Date -> DateTime,
    Time -> DateTime,
    Timestamp -> DateTime,
}
