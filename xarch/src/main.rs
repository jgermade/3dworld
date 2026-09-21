//! The native half. Prints the rows as TSV, one per line, with every value
//! carrying its own bit pattern — `tools/xarch.py` compares those and not the
//! printed decimals.

fn main() {
    for (i, row) in w3d_xarch::measurements().into_iter().enumerate() {
        println!(
            "{i}\t{}\t{}\t{:016x}\t{}",
            row.name,
            row.rule.as_str(),
            row.value.to_bits(),
            row.value
        );
    }
}
