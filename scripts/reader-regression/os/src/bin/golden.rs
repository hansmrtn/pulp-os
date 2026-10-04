// prints the golden trace (see src/golden.rs); scripts/check-reader-regression.sh hashes it
fn main() {
    print!("{}", pulp_os_host::golden::trace());
}
