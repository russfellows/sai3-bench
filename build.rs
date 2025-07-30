//
fn main() {
    println!("cargo:rerun-if-changed=proto/s3bench.proto");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/pb") // generate into a real, tracked path
        .compile_protos(&["proto/s3bench.proto"], &["proto"])
        .unwrap();
}

