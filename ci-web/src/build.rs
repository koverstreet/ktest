extern crate capnpc;

fn main() {
    capnpc::CompilerCommand::new()
        .output_path(".")
        .file("src/testresult.capnp")
        .run()
        .expect("compiling schema");
}
