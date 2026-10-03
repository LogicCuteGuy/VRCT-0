//! Stable public error codes/messages shared with the frontend.
use std::sync::OnceLock;
use serde_json::{json,Value};
use crate::router::Reply;
pub fn reply(code:&str,data:Value,message:Option<&str>)->Reply {
    static TABLE:OnceLock<Value>=OnceLock::new();
    let metadata=&TABLE.get_or_init(||serde_json::from_str(include_str!("../assets/errors.json")).expect("error metadata"))[code];
    (400,json!({"error_code":code,"message":message.map(Value::from).unwrap_or_else(||metadata["message"].clone()),
        "data":data,"details":{},"category":metadata["category"],"severity":metadata["severity"]}))
}
