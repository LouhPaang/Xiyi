// intrinsic.rs
use crate::ast::Type;
use std::collections::HashMap;
use std::sync::LazyLock;

/// 内建函数元数据（结构体保持不变）
#[derive(Debug, Clone)]
pub struct Intrinsic {
    pub name: &'static str,
    pub allowed_in_model: bool,
    pub is_pure: bool,
    pub signature: Option<Vec<Type>>,
    pub return_type: Option<Type>,
    pub link_name: &'static str,
    pub doc: &'static str,
}

/// 全局内建函数表（惰性初始化）
static INTRINSICS: LazyLock<HashMap<&'static str, Intrinsic>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // 1. print
    m.insert(
        "print",
        Intrinsic {
            name: "print",
            allowed_in_model: false,
            is_pure: false,
            signature: Some(vec![Type::Str]),
            return_type: Some(Type::Unit),
            link_name: "xiyi::io::print",
            doc: "Prints a value to the standard output",
        },
    );

    // 2. panic
    m.insert(
        "panic",
        Intrinsic {
            name: "panic",
            allowed_in_model: false,
            is_pure: false,
            signature: Some(vec![Type::Str]),
            return_type: Some(Type::Unit), // 实际为 never，但 MIR 用 Unit 占位
            link_name: "core::panic",
            doc: "Panics with a given message",
        },
    );

    // 3. from_utf8_unchecked
    m.insert(
        "from_utf8_unchecked",
        Intrinsic {
            name: "from_utf8_unchecked",
            allowed_in_model: true,
            is_pure: true,
            signature: Some(vec![Type::Ref {
                mutable: false,
                inner: Box::new(Type::Slice(Box::new(Type::U8))),
            }]),
            return_type: Some(Type::Ref {
                mutable: false,
                inner: Box::new(Type::Str),
            }),
            link_name: "core::str::from_utf8_unchecked",
            doc: "Converts a &[u8] to &str without validation (unsafe)",
        },
    );

    // 4. tensor.cond
    m.insert(
        "tensor.cond",
        Intrinsic {
            name: "tensor.cond",
            allowed_in_model: true,
            is_pure: true,
            signature: None,
            return_type: None,
            link_name: "xiyi_tensor::cond",
            doc: "Dynamic conditional operator in graph domain",
        },
    );

    // 5. tensor.while_loop
    m.insert(
        "tensor.while_loop",
        Intrinsic {
            name: "tensor.while_loop",
            allowed_in_model: true,
            is_pure: true,
            signature: None,
            return_type: None,
            link_name: "xiyi_tensor::while_loop",
            doc: "Dynamic while loop operator in graph domain",
        },
    );

    // 6. linear
    m.insert(
        "linear",
        Intrinsic {
            name: "linear",
            allowed_in_model: true,
            is_pure: true,
            signature: None,
            return_type: None,
            link_name: "xiyi_math::linear",
            doc: "Tensor linear transformation",
        },
    );

    // 7. 基础类型关联常量（示例）
    m.insert(
        "i128::MAX",
        Intrinsic {
            name: "i128::MAX",
            allowed_in_model: true,
            is_pure: true,
            signature: None,
            return_type: Some(Type::I128),
            link_name: "i128::MAX",
            doc: "Maximum value of i128",
        },
    );

    // 你可以继续添加其余内建函数（conv2d、relu 等），格式同上。
    // 如果只是供 codegen 查询，也可以只注册必要的几个。

    m
});

/// 查询内建函数
pub fn get_intrinsic(name: &str) -> Option<&'static Intrinsic> {
    INTRINSICS.get(name)
}

/// 判断是否为内建函数
pub fn is_intrinsic(name: &str) -> bool {
    INTRINSICS.contains_key(name)
}

/// 返回所有内建函数名称列表（供后端收集）
pub fn all_intrinsic_names() -> Vec<&'static str> {
    INTRINSICS.keys().copied().collect()
}