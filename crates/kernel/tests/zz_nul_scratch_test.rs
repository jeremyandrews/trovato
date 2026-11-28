#[test]
fn nul_byte_behavior_in_ammonia() {
    let input = "hello\u{0000}world<b>x</b>";
    let cleaned = ammonia::Builder::default()
        .tags(std::collections::HashSet::new())
        .clean(input)
        .to_string();
    eprintln!("input bytes: {:?}", input.as_bytes());
    eprintln!("output bytes: {:?}", cleaned.as_bytes());
    eprintln!("output: {:?}", cleaned);
    eprintln!("contains NUL: {}", cleaned.contains('\u{0000}'));
    eprintln!("contains U+FFFD: {}", cleaned.contains('\u{FFFD}'));
}
