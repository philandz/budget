use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (proto_root, proto_prefix, libs_prefix) =
        if Path::new("../protobuf/budget/budget.proto").exists() {
            ("..", "../protobuf", "../libs")
        } else {
            (".", "protobuf", "libs")
        };

    let mut includes = vec![proto_root.to_string()];
    for candidate in [
        "/usr/include",
        "/usr/local/include",
        "/opt/homebrew/include",
    ] {
        if Path::new(candidate).exists() {
            includes.push(candidate.to_string());
        }
    }

    if let Ok(extra_include) = std::env::var("PROTOC_INCLUDE") {
        if !extra_include.is_empty() && Path::new(&extra_include).exists() {
            includes.push(extra_include);
        }
    }

    let include_refs: Vec<&str> = includes.iter().map(String::as_str).collect();

    let files = [
        format!("{proto_prefix}/budget/budget.proto"),
        format!("{proto_prefix}/identity/identity.proto"),
        format!("{proto_prefix}/shared/user/user.proto"),
        format!("{proto_prefix}/shared/organization/organization.proto"),
        format!("{libs_prefix}/protobuf/common/base.proto"),
    ];
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&file_refs, &include_refs)?;
    Ok(())
}
