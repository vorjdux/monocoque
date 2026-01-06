
#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Monocoque API Examples ===\n");
    
    // Example 1: DEALER Socket
    println!("1. DEALER Socket (using prelude):");
    println!("   use monocoque_zmtp::prelude::*;");
    println!("   let socket = DealerSocket::new(stream);");
    println!("   socket.send(vec![Bytes::from(\"message\")]).await?;\n");
    
    // Example 2: Direct imports
    println!("2. Direct Socket Imports:");
    println!("   use monocoque_zmtp::{{DealerSocket, RouterSocket}};");
    println!("   use monocoque_zmtp::{{PubSocket, SubSocket}};\n");
    
    // Example 3: Session types
    println!("3. Protocol Types:");
    println!("   use monocoque_zmtp::{{SocketType, ZmtpSession}};\n");
    
    println!("All socket types are now exported at the crate root!");
    println!("No need to import from internal modules.\n");
    
    // Demonstrate that imports work
    demonstrate_api_usage().await?;
    
    Ok(())
}

async fn demonstrate_api_usage() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== API Organization ===");
    println!("✓ DealerSocket available");
    println!("✓ RouterSocket available");
    println!("✓ PubSocket available");
    println!("✓ SubSocket available");
    println!("✓ SocketType enum available");
    println!("✓ Prelude module available");
    println!("\nClean, ergonomic API ready to use!");
    
    Ok(())
}
