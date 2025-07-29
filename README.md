# Overview
The plan for this tool is to create a basic, S3 testing tool, similar to MinIO's warp.  However, this tool leverages the [s3dlio Rust library project](https://github.com/russfellows/s3dlio)

So, what is different, and / or better?  In other words, why do we need this project?

1. This uses the AWS, S3 Rust SDK (warp does not)
2. This supports replay of captured workloads (warp, does not)
3. Support for settable data dedupe and compression levels (again, no in warp)
4. Better output logging and result analysis tools (warp analyze is hiddeously slow and difficult to obtain data as desired)


## Initial Plan
The initial plan for this project is captured in a [Discussion item #2](https://github.com/russfellows/warp-test/discussions/2)


