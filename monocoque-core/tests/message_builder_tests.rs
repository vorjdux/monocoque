//! Integration tests for Message builder API

use monocoque_core::message_builder::Message;
use bytes::Bytes;

#[test]
fn test_message_builder_basic() {
    let msg = Message::new()
        .push_str("topic")
        .push_str("Hello")
        .push(Vec::from(&b"World"[..]));

    assert_eq!(msg.len(), 3);
    
    let frames = msg.into_frames();
    assert_eq!(frames[0], Bytes::from_static(b"topic"));
    assert_eq!(frames[1], Bytes::from_static(b"Hello"));
    assert_eq!(frames[2], Bytes::from_static(b"World"));
}

#[test]
fn test_message_builder_empty_frames() {
    let msg = Message::new()
        .push(Vec::from(&b"identity"[..]))
        .push_empty()
        .push_str("body");

    let frames = msg.into_frames();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], Bytes::from_static(b"identity"));
    assert!(frames[1].is_empty());
    assert_eq!(frames[2], Bytes::from_static(b"body"));
}

#[test]
fn test_message_builder_integers() {
    let msg = Message::new()
        .push_u32(12345)
        .push_u64(67890);

    let frames = msg.into_frames();
    assert_eq!(frames.len(), 2);
    
    let val32 = u32::from_be_bytes(frames[0].as_ref().try_into().unwrap());
    assert_eq!(val32, 12345);
    
    let val64 = u64::from_be_bytes(frames[1].as_ref().try_into().unwrap());
    assert_eq!(val64, 67890);
}

#[test]
fn test_message_builder_capacity() {
    let msg = Message::with_capacity(10);
    assert_eq!(msg.len(), 0);
    assert!(msg.is_empty());  // Pre-allocated capacity but no frames yet
    
    let msg = msg
        .push_str("frame1")
        .push_str("frame2");
    
    assert_eq!(msg.len(), 2);
}

#[test]
fn test_message_builder_from_frames() {
    let frames = vec![
        Bytes::from_static(b"a"),
        Bytes::from_static(b"b"),
        Bytes::from_static(b"c"),
    ];
    
    let msg = Message::from_frames(frames.clone());
    assert_eq!(msg.len(), 3);
    assert_eq!(msg.frames(), &frames[..]);
}

#[test]
fn test_message_builder_conversions() {
    let frames = vec![Bytes::from_static(b"test")];
    
    // From Vec<Bytes>
    let msg: Message = frames.clone().into();
    assert_eq!(msg.len(), 1);
    
    // Into Vec<Bytes>
    let result: Vec<Bytes> = msg.into();
    assert_eq!(result, frames);
}

#[test]
fn test_message_builder_chaining() {
    // Test fluent API chaining
    let msg = Message::new()
        .push_str("a")
        .push_str("b")
        .push_empty()
        .push(Vec::from(&b"c"[..]))
        .push_u32(100);
    
    assert_eq!(msg.len(), 5);
}
