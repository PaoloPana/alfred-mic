
#[allow(clippy::cast_precision_loss)]
pub const fn usize_to_f64_unchecked(val: usize) -> f64 {
    val as f64
}

#[allow(clippy::cast_precision_loss)]
pub const fn i64_to_f64_unchecked(val: i64) -> f64 {
    val as f64
}

#[allow(clippy::cast_possible_truncation)]
pub const fn f64_to_i64_unchecked(val: f64) -> i64 {
    val as i64
}