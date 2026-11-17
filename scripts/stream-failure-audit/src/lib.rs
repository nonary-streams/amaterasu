//! Conservative, syntax-aware failure inventory. This is deliberately not a
//! compiler, a proof of reachability, or a policy enforcement mechanism.
pub mod graph;
pub mod report;
pub mod scan;

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Location {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub kind: String,
    pub location: Location,
    pub expression: String,
    pub evidence: String,
    pub guards: Vec<String>,
    pub literals: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Call {
    pub name: String,
    pub qualifier: String,
    pub receiver_type: Option<String>,
    pub method: bool,
    pub location: Location,
}

#[derive(Clone, Debug, Serialize)]
pub struct Function {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub namespace: String,
    pub package: String,
    pub location: Location,
    // Serialized once in findings, rather than duplicating large expressions.
    #[serde(skip_serializing)]
    pub observations: Vec<Observation>,
    pub calls: Vec<Call>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Inventory {
    pub functions: Vec<Function>,
    pub files: Vec<SourceFile>,
    pub excluded: Vec<String>,
    pub gaps: Vec<String>,
    pub errors: Vec<String>,
    pub api_metadata: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
    pub functions: usize,
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
