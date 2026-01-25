#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Test ZMTP greeting parsing - the first 64 bytes of the handshake
    if data.len() >= 64 {
        // Try to parse a greeting
        let _ = parse_greeting(data);
    }
    
    // Test command parsing
    if data.len() >= 2 {
        let _ = parse_command(data);
    }
});

// Simple greeting parser to test - based on ZMTP spec
fn parse_greeting(data: &[u8]) -> Result<(), &'static str> {
    if data.len() < 64 {
        return Err("Too short");
    }
    
    // Check signature
    if data[0] != 0xff || data[9] != 0x7f {
        return Err("Invalid signature");
    }
    
    // Check protocol version
    let major = data[10];
    let minor = data[11];
    if major != 3 || (minor != 0 && minor != 1) {
        return Err("Unsupported version");
    }
    
    Ok(())
}

// Simple command parser
fn parse_command(data: &[u8]) -> Result<(), &'static str> {
    if data.len() < 2 {
        return Err("Too short");
    }
    
    let flags = data[0];
    let size = data[1];
    
    // Check if it's a long command
    if (flags & 0x02) != 0 {
        if data.len() < 9 {
            return Err("Long command too short");
        }
    }
    
    Ok(())
}
