// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
//! Lossless arguments for the Firebird schema-engine JavaScript adapter bridge.

use crate::conversion::{JSArg, JSArgType};
use quaint::{Value, ValueType, chrono::Timelike};
use serde::ser::Error as _;
use serde_json::Value as JsonValue;

pub fn value_to_js_arg(value: &Value<'_>) -> serde_json::Result<JSArg> {
    Ok(match &value.typed {
        ValueType::Int32(Some(value)) => JSArg::SafeInt(*value),
        ValueType::Int64(Some(value)) => JSArg::Value(JsonValue::String(value.to_string())),
        ValueType::Numeric(Some(value)) => JSArg::Value(JsonValue::String(value.to_string())),
        ValueType::Bytes(Some(value)) => JSArg::Buffer(value.to_vec()),
        ValueType::DateTime(Some(value)) => {
            // Migration persistence uses Utc::now(), while Firebird 3 stores 100-microsecond ticks.
            let value = value.with_nanosecond(value.nanosecond() / 100_000 * 100_000).unwrap();
            JSArg::Value(JsonValue::String(value.to_rfc3339()))
        }
        ValueType::Time(Some(value)) => {
            let value = value.with_nanosecond(value.nanosecond() / 100_000 * 100_000).unwrap();
            JSArg::Value(JsonValue::String(value.to_string()))
        }
        ValueType::Array(_)
        | ValueType::EnumArray(_, _)
        | ValueType::Enum(_, _)
        | ValueType::Json(_)
        | ValueType::Xml(_)
        | ValueType::Opaque(_) => {
            return Err(serde_json::Error::custom(
                "Firebird schema operations do not support array, enum, JSON, XML or opaque parameters",
            ));
        }
        value => JSArg::from(JsonValue::from(value.clone())),
    })
}

pub fn value_to_js_arg_type(value: &Value<'_>) -> JSArgType {
    let mut arg_type = super::value_to_js_arg_type(value);
    if arg_type.db_type.is_none() {
        arg_type.db_type = match &value.typed {
            ValueType::Date(_) => Some("DATE".to_owned()),
            ValueType::Time(_) => Some("TIME".to_owned()),
            ValueType::DateTime(_) => Some("TIMESTAMP".to_owned()),
            _ => None,
        };
    }
    arg_type
}

#[cfg(test)]
mod tests {
    use super::*;
    use quaint::{
        bigdecimal::BigDecimal,
        chrono::{TimeZone, Utc},
    };
    use std::str::FromStr;

    #[test]
    fn exact_numbers_are_strings_and_binary_values_are_buffers() {
        assert_eq!(
            value_to_js_arg(&Value::int64(i64::MAX)).unwrap(),
            JSArg::Value(JsonValue::String(i64::MAX.to_string()))
        );
        assert_eq!(
            value_to_js_arg(&Value::numeric(BigDecimal::from_str("1234567890123456.78").unwrap())).unwrap(),
            JSArg::Value(JsonValue::String("1234567890123456.78".to_owned()))
        );
        assert_eq!(
            value_to_js_arg(&Value::bytes(vec![0, 255])).unwrap(),
            JSArg::Buffer(vec![0, 255])
        );
    }

    #[test]
    fn migration_timestamps_use_firebird_precision_and_native_type_metadata() {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 10, 18, 10, 20, 30)
            .unwrap()
            .with_nanosecond(123_456_789)
            .unwrap();
        let value = Value::datetime(timestamp);
        assert_eq!(
            value_to_js_arg(&value).unwrap(),
            JSArg::Value(JsonValue::String("2026-10-18T10:20:30.123400+00:00".to_owned()))
        );
        assert_eq!(value_to_js_arg_type(&value).db_type.as_deref(), Some("TIMESTAMP"));
    }
}
