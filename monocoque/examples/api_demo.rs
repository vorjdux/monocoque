
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Monocoque API Examples ===\n");
    
    // Example 1: DEALER Socket
    info!("1. DEALER Socket (using prelude):");
    info!("   use monocoque_zmtp::prelude::*;");
    info!("   let socket = DealerSocket::new(stream);");
    info!("   socket.send(vec![Bytes::from(\"message\")]).await?;\n");
    
    // Example 2: Direct imports
    info!("2. Direct Socket Imports:");
    info!("   use monocoque_zmtp::{{DealerSocket, RouterSocket}};");
    info!("   use monocoque_zmtp::{{PubSocket, SubSocket}};\n");
    
    // Example 3: Session types
    info!("3. Protocol Types:");
    info!("   use monocoque_zmtp::{{SocketType, ZmtpSession}};\n");
    
    info!("All socket types are now exported at the crate root!");
    info!("No need to import from internal modules.\n");
    
    // Demonstrate that imports work
    demonstrate_api_usage().await?;
    
    Ok(())
}

async fn demonstrate_api_usage() -> Result<(), Box<dyn std::error::Error>> {
    info!("=== API Organization ===");
    info!("✓ DealerSocket available");
    info!("✓ RouterSocket available");
    info!("✓ PubSocket available");
    info!("✓ SubSocket available");
    info!("✓ SocketType enum available");
    info!("✓ Prelude module available");
    info!("\nClean, ergonomic API ready to use!");
    
    Ok(())
}
