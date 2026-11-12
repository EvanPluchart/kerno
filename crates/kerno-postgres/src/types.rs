//! Mapping PostgreSQL types to Kerno's type families.

use kerno_core::TypeKind;
use tokio_postgres::types::{Kind, Type};

/// The family of a base type, by its catalog name (`pg_type.typname`).
pub(crate) fn kind_of_base(type_name: &str) -> TypeKind {
    match type_name {
        "int2" | "int4" | "int8" | "oid" | "xid" | "xid8" | "cid" => TypeKind::Integer,
        "float4" | "float8" => TypeKind::Float,
        "numeric" | "money" => TypeKind::Decimal,
        "bool" => TypeKind::Boolean,
        "text" | "varchar" | "bpchar" | "name" | "char" | "citext" | "xml" => TypeKind::Text,
        "bytea" => TypeKind::Binary,
        "date" => TypeKind::Date,
        "time" | "timetz" => TypeKind::Time,
        "timestamp" => TypeKind::DateTime,
        "timestamptz" => TypeKind::DateTimeTz,
        "interval" => TypeKind::Interval,
        "uuid" => TypeKind::Uuid,
        "json" | "jsonb" => TypeKind::Json,
        "bit" | "varbit" => TypeKind::Bit,
        _ => TypeKind::Other,
    }
}

/// The family of a catalog type, from `typname`, `typtype` and
/// `typcategory`, with the base type's name for domains.
pub(crate) fn kind_of_catalog(
    type_name: &str,
    type_type: &str,
    category: &str,
    domain_base: Option<&str>,
) -> TypeKind {
    // The category decides "is an array": attndims and name prefixes are
    // not reliable.
    if category == "A" {
        return TypeKind::Array;
    }
    match type_type {
        "e" => TypeKind::Enum,
        "d" => domain_base.map_or(TypeKind::Other, kind_of_base),
        _ => kind_of_base(type_name),
    }
}

/// The family and display name of a result column's type.
pub(crate) fn describe(ty: &Type) -> (String, TypeKind) {
    match ty.kind() {
        Kind::Array(element) => (format!("{}[]", describe(element).0), TypeKind::Array),
        Kind::Enum(_) => (ty.name().to_string(), TypeKind::Enum),
        Kind::Domain(base) => (ty.name().to_string(), describe(base).1),
        _ => (ty.name().to_string(), kind_of_base(ty.name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_types_map_to_their_family() {
        assert_eq!(describe(&Type::INT8), ("int8".into(), TypeKind::Integer));
        assert_eq!(
            describe(&Type::NUMERIC),
            ("numeric".into(), TypeKind::Decimal)
        );
        assert_eq!(describe(&Type::BYTEA), ("bytea".into(), TypeKind::Binary));
        assert_eq!(
            describe(&Type::TIMESTAMPTZ),
            ("timestamptz".into(), TypeKind::DateTimeTz)
        );
        assert_eq!(describe(&Type::JSONB), ("jsonb".into(), TypeKind::Json));
        assert_eq!(
            describe(&Type::INT4_ARRAY),
            ("int4[]".into(), TypeKind::Array)
        );
        assert_eq!(
            describe(&Type::TEXT_ARRAY),
            ("text[]".into(), TypeKind::Array)
        );
        assert_eq!(describe(&Type::POINT), ("point".into(), TypeKind::Other));
    }

    #[test]
    fn catalog_types_use_category_and_domain_base() {
        assert_eq!(kind_of_catalog("_int4", "b", "A", None), TypeKind::Array);
        assert_eq!(
            kind_of_catalog("order_status", "e", "E", None),
            TypeKind::Enum
        );
        assert_eq!(
            kind_of_catalog("email", "d", "S", Some("text")),
            TypeKind::Text
        );
        assert_eq!(kind_of_catalog("address", "c", "C", None), TypeKind::Other);
        assert_eq!(kind_of_catalog("uuid", "b", "U", None), TypeKind::Uuid);
    }
}
