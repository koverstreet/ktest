extern crate capnpc;

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
}
