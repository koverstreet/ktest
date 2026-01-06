extern crate capnpc;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn main() {
    capnpc::CompilerCommand::new()
        .output_path(".")
        .file("src/testresult.capnp")
        .run()
        .expect("compiling schema");

    capnpc::CompilerCommand::new()
        .output_path(".")
        .file("src/worker.capnp")
        .run()
        .expect("compiling schema");

    capnpc::CompilerCommand::new()
        .output_path(".")
        .file("src/durations.capnp")
        .run()
        .expect("compiling schema");

    generate_lustre_sanity_tests();
}

fn generate_lustre_sanity_tests() {
    let template_path = "tests/fs/lustre/sanity/sanity.ktest.in";
    let output_dir = "tests/fs/lustre/sanity";

    if !Path::new(template_path).exists() {
        return;
    }

    let template_content =
        fs::read_to_string(template_path).expect("Failed to read sanity.ktest.in template");

    for i in 1..=50 {
        let output_path = format!("{}/sanity-{}.ktest", output_dir, i);
        let content = template_content
            .replace("INDEX", &i.to_string())
            .replace("BATCH", "10");

        fs::write(&output_path, content)
            .unwrap_or_else(|e| panic!("Failed to write {}: {}", output_path, e));

        // Set executable bit (mode 0755)
        let file = fs::File::open(&output_path)
            .unwrap_or_else(|e| panic!("Failed to open {}: {}", output_path, e));
        let mut perms = file
            .metadata()
            .unwrap_or_else(|e| panic!("Failed to get metadata for {}: {}", output_path, e))
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&output_path, perms)
            .unwrap_or_else(|e| panic!("Failed to set permissions for {}: {}", output_path, e));
    }

    println!("cargo:rerun-if-changed={}", template_path);
}
