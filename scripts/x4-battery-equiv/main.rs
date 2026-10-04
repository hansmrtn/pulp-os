// Prints X4 battery conversion results for every u16 input (T9 equivalence).
use harness::drivers::battery;

fn main() {
    for mv in 0..=u16::MAX {
        println!(
            "{} {} {}",
            mv,
            battery::adc_to_battery_mv(mv),
            battery::battery_percentage(mv)
        );
    }
}
