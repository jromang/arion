fn main() {
    let devices = thetis_audio::enumerate_output_devices();
    println!("Found {} output devices:", devices.len());
    for (i, name) in devices.iter().enumerate() {
        println!("  {i}: {name}");
    }
}
