pub fn value(name: &str) -> serde_json::Value {
    serde_json::json!({"name":name,"version":env!("CARGO_PKG_VERSION"),"api_version":super::ipc::API_VERSION,"build_sha":env!("P2P_BUILD_SHA"),"target":env!("P2P_BUILD_TARGET"),"source_state":env!("P2P_BUILD_STATE")})
}
