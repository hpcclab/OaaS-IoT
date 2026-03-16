use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
#[derive(Default)]
pub enum FunctionType {
    #[serde(rename = "BUILTIN")]
    Builtin,
    #[serde(rename = "CUSTOM")]
    #[default]
    Custom,
    #[serde(rename = "MACRO")]
    Macro,
    #[serde(rename = "LOGICAL")]
    Logical,
    #[serde(rename = "WASM")]
    Wasm,
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
#[derive(Default)]
pub enum DeploymentCondition {
    #[serde(rename = "PENDING")]
    #[default]
    Pending,
    #[serde(rename = "DEPLOYING")]
    Deploying,
    #[serde(rename = "RUNNING")]
    Running,
    #[serde(rename = "DOWN")]
    Down,
    #[serde(rename = "DELETED")]
    Deleted,
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
#[derive(Default)]
pub enum FunctionAccessModifier {
    #[serde(rename = "PUBLIC")]
    #[default]
    Public,
    #[serde(rename = "INTERNAL")]
    Internal,
    #[serde(rename = "PRIVATE")]
    Private,
}


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
#[derive(Default)]
pub enum ConsistencyModel {
    #[serde(rename = "NONE")]
    #[default]
    None,
    #[serde(rename = "READ_YOUR_WRITE")]
    ReadYourWrite,
    #[serde(rename = "BOUNDED_STALENESS")]
    BoundedStaleness,
    #[serde(rename = "STRONG")]
    Strong,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_type_wasm_serializes_to_wasm_string() {
        let json = serde_json::to_string(&FunctionType::Wasm).unwrap();
        assert_eq!(json, r#""WASM""#);
    }

    #[test]
    fn function_type_wasm_deserializes_from_wasm_string() {
        let ft: FunctionType = serde_json::from_str(r#""WASM""#).unwrap();
        assert_eq!(ft, FunctionType::Wasm);
    }

    #[test]
    fn function_type_default_is_custom() {
        assert_eq!(FunctionType::default(), FunctionType::Custom);
    }

    #[test]
    fn function_type_roundtrip_all_variants() {
        let variants = vec![
            FunctionType::Builtin,
            FunctionType::Custom,
            FunctionType::Macro,
            FunctionType::Logical,
            FunctionType::Wasm,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let back: FunctionType = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }
}
