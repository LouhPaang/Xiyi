use xiyi_compiler::parser::Parser;
use xiyi_compiler::sema::TypeChecker;
use xiyi_compiler::ast::Item;
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: xiyi [--stdlib <path>] <file.xiyi>");
        std::process::exit(1);
    }

    let mut filename = None;
    let mut stdlib_path = String::from("Standard/");

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--stdlib" => {
                if i + 1 < args.len() {
                    stdlib_path = args[i + 1].clone();
                    i += 2;
                } else {
                    eprintln!("--stdlib requires a path argument");
                    std::process::exit(1);
                }
            }
            _ => {
                if filename.is_none() && !args[i].starts_with("--") {
                    filename = Some(args[i].clone());
                }
                i += 1;
            }
        }
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            eprintln!("No input file specified");
            std::process::exit(1);
        }
    };

    // 1. 加载用户源码
    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading file {}: {}", filename, e);
            std::process::exit(1);
        }
    };

    // 2. 加载标准库（带来源追踪，用于冲突检测）
    let stdlib_items = match load_stdlib(&stdlib_path) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("Failed to load standard library: {}", e);
            std::process::exit(1);
        }
    };

    // 3. 解析用户源码
    let mut parser = Parser::new(&source);
    let user_program = match parser.parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Parse error: {}", e);
            std::process::exit(1);
        }
    };

    // 4. 合并标准库和用户程序（带命名冲突检测）
    let program = match merge_with_conflict_check(stdlib_items, user_program.items, &filename) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    // 5. 类型检查 + HIR
    let mut checker = TypeChecker::new();
    let hir = match checker.check_program(&program) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Type error: {}", e);
            std::process::exit(1);
        }
    };

    // 6. 展开语法糖（for、? 等）
    let hir = xiyi_compiler::elaborator::Elaborator::elaborate(hir);

    // 7. 构建 MIR
    let mir = xiyi_compiler::mir_builder::MirBuilder::build(&hir);

    // 8. MIR 优化（常量折叠、死代码消除）
    let mir = xiyi_compiler::simplify::Simplify::run(mir);

    // 9. 代码生成
    let rust_code = xiyi_compiler::codegen::Codegen::generate_from_mir(&mir);

    // 10. 构建输出目录并编译
    let temp_dir = std::path::PathBuf::from("D:\\xiyi_build");
    if let Err(e) = fs::create_dir_all(&temp_dir) {
        eprintln!("Failed to create temp dir: {}", e);
        std::process::exit(1);
    }

    // 注释掉 tch 依赖，避免 torch-sys 编译失败
    let cargo_toml = r#"[package]
name = "xiyi_output"
version = "0.1.0"
edition = "2024"

[dependencies]
# tch = { version = "0.13", features = ["download-libtorch"] }

[[bin]]
name = "xiyi_output"
path = "src/main.rs"
"#;
    if let Err(e) = fs::write(temp_dir.join("Cargo.toml"), cargo_toml) {
        eprintln!("Failed to write Cargo.toml: {}", e);
        std::process::exit(1);
    }

    let src_dir = temp_dir.join("src");
    if let Err(e) = fs::create_dir_all(&src_dir) {
        eprintln!("Failed to create src dir: {}", e);
        std::process::exit(1);
    }
    if let Err(e) = fs::write(src_dir.join("main.rs"), rust_code) {
        eprintln!("Failed to write main.rs: {}", e);
        std::process::exit(1);
    }

    // 8. cargo build
    let status = Command::new("cargo")
        .arg("build")
        .current_dir(&temp_dir)
        .status();

    match status {
        Ok(status) if status.success() => {
            let exe_path = if cfg!(windows) {
                temp_dir.join("target").join("debug").join("xiyi_output.exe")
            } else {
                temp_dir.join("target").join("debug").join("xiyi_output")
            };
            if !exe_path.exists() {
                eprintln!("Executable not found after build");
                std::process::exit(1);
            }
            let run_status = Command::new(exe_path).status();
            match run_status {
                Ok(status) => std::process::exit(status.code().unwrap_or(0)),
                Err(e) => {
                    eprintln!("Failed to run executable: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Ok(_) => {
            eprintln!("Cargo build failed");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to run cargo: {}", e);
            std::process::exit(1);
        }
    }
}

fn item_name(item: &Item) -> Option<&str> {
    match item {
        Item::FnDef(f) => Some(&f.name),
        Item::StructDef(s) => Some(&s.name),
        Item::EnumDef(e) => Some(&e.name),
        Item::ConstDef(c) => Some(&c.name),
        Item::ModelDef(m) => Some(&m.name),
        Item::ProtoDef(p) => Some(&p.name),
        Item::Interface(iface) => Some(&iface.name),
        Item::Use(_) => None,
        Item::Implement(_) => None,
    }
}

fn load_stdlib(stdlib_path: &str) -> Result<Vec<Item>, String> {
    let core_src = Path::new(stdlib_path).join("xiyi-core").join("src");

    if !core_src.is_dir() {
        return Err(format!(
            "stdlib source directory not found: {}",
            core_src.display()
        ));
    }

    // 收集所有 .xiyi 文件，lib.xiyi 单独放到最后加载
    let mut module_files: Vec<std::path::PathBuf> = Vec::new();
    let mut lib_file: Option<std::path::PathBuf> = None;

    let entries = fs::read_dir(&core_src)
        .map_err(|e| format!("Failed to read {}: {}", core_src.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read dir entry: {}", e))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("xiyi") {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some("lib.xiyi") {
            lib_file = Some(path);
        } else {
            module_files.push(path);
        }
    }
    // 按文件名排序，保证跨平台、跨运行的加载顺序确定
    module_files.sort();
    if let Some(lib) = lib_file {
        module_files.push(lib);
    }

    if module_files.is_empty() {
        // 允许空标准库，返回空 Vec
        return Ok(Vec::new());
    }

    let mut all_items: Vec<Item> = Vec::new();
    // 记录每个已定义名字来自哪个模块文件，用于冲突检测和报错定位
    let mut defined_in: HashMap<String, String> = HashMap::new();

    for file_path in &module_files {
        let module_label = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        let source = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

        let mut parser = Parser::new(&source);
        let program = parser
            .parse_program()
            .map_err(|e| format!("Parse error in {}: {}", module_label, e))?;

        for item in program.items {
            if let Some(name) = item_name(&item) {
                if let Some(prev_module) = defined_in.get(name) {
                    return Err(format!(
                        "duplicate definition `{}` in stdlib: defined in both `{}` and `{}`",
                        name, prev_module, module_label
                    ));
                }
                defined_in.insert(name.to_string(), module_label.clone());
            }
            all_items.push(item);
        }
    }

    Ok(all_items)
}

fn merge_with_conflict_check(
    stdlib_items: Vec<Item>,
    user_items: Vec<Item>,
    user_filename: &str,
) -> Result<xiyi_compiler::ast::Program, String> {
    let stdlib_names: std::collections::HashSet<String> = stdlib_items
        .iter()
        .filter_map(item_name) // 只保留有名字的
        .map(|s| s.to_string())
        .collect();

    for item in &user_items {
        if let Some(name) = item_name(item) {
            if stdlib_names.contains(name) {
                return Err(format!(
                    "error: `{}` in {} conflicts with a standard library definition of the same name.",
                    name, user_filename
                ));
            }
        }
    }

    let mut all_items = stdlib_items;
    all_items.extend(user_items);
    Ok(xiyi_compiler::ast::Program { items: all_items })
}