//! Build script: records the git commit this app was built from as `CAMERATA_GIT_COMMIT`, exactly
//! as the server's does, so the app can compare it with the one the server reports on
//! `/api/health` (see `src/build_guard.rs`). The logic is the server's own file, included by path,
//! so the two binaries can never name their build differently.

#[path = "../server/build.rs"]
mod build_identity;

fn main() {
    build_identity::main();
}
