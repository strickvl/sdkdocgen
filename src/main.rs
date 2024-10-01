use clap::Parser;
use rustpython_parser::{ast, Parse};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the Python file
    #[arg(short, long)]
    file: String,

    /// Output directory for the Markdown file
    #[arg(short, long)]
    output_path: PathBuf,
}

fn is_classmethod(decorators: &[ast::Expr]) -> bool {
    decorators.iter().any(|d| {
        if let ast::Expr::Name(ast::ExprName { id, .. }) = d {
            id == "classmethod"
        } else {
            false
        }
    })
}

fn extract_type(annotation: &Box<ast::Expr>) -> String {
    match &**annotation {
        ast::Expr::Name(name) => name.id.to_string(),
        ast::Expr::Attribute(attr) => format!(
            "{}.{}",
            extract_type(&Box::new(*attr.value.clone())),
            attr.attr
        ),
        ast::Expr::Subscript(subscript) => {
            let value_type = extract_type(&Box::new(*subscript.value.clone()));
            let slice_type = match &*subscript.slice {
                ast::Expr::Tuple(tuple) => {
                    let types: Vec<String> = tuple
                        .elts
                        .iter()
                        .map(|k| extract_type(&Box::new(k.clone())))
                        .collect();
                    types.join(", ")
                }
                _ => extract_type(&Box::new(*subscript.slice.clone())),
            };
            format!("{}[{}]", value_type, slice_type)
        }
        ast::Expr::List(list) => {
            let elements: Vec<String> = list
                .elts
                .iter()
                .map(|e| extract_type(&Box::new(e.clone())))
                .collect();
            format!("[{}]", elements.join(", "))
        }
        ast::Expr::Tuple(tuple) => {
            let elements: Vec<String> = tuple
                .elts
                .iter()
                .map(|e| extract_type(&Box::new(e.clone())))
                .collect();
            format!("({})", elements.join(", "))
        }
        ast::Expr::Call(call) => {
            let func_name = extract_type(&call.func);
            let args: Vec<String> = call
                .args
                .iter()
                .map(|arg| extract_type(&Box::new(arg.clone())))
                .collect();
            format!("{}[{}]", func_name, args.join(", "))
        }
        ast::Expr::BinOp(binop) => {
            let left = extract_type(&binop.left);
            let right = extract_type(&binop.right);
            format!("{} | {}", left, right) // Assuming '|' is used for Union types
        }
        // If we encounter any other type that we haven't explicitly handled,
        // we'll return it as a string representation
        _ => format!("{:?}", annotation),
    }
}

fn format_args_table(args: &ast::Arguments) -> String {
    let mut table = String::from(
        "\n**Parameters:**\n\n| Name | Type | Description | Default |\n| --- | --- | --- | --- |\n",
    );

    let args_len = args.args.len();
    let defaults: Vec<&ast::Expr> = args.defaults().collect();
    let defaults_len = defaults.len();

    for (i, arg) in args.args.iter().enumerate() {
        let name = &arg.def.arg;
        let arg_type = arg
            .def
            .annotation
            .as_ref()
            .map_or("Any".to_string(), extract_type);
        let description = ""; // You'd need to extract this from the docstring
        let default = if i >= args_len - defaults_len {
            let default_index = i - (args_len - defaults_len);
            format!("{:?}", defaults[default_index])
        } else {
            "_required_".to_string()
        };

        table.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            name, arg_type, description, default
        ));
    }

    // Handle keyword-only arguments
    for (_i, arg) in args.kwonlyargs.iter().enumerate() {
        let name = &arg.def.arg;
        let arg_type = arg
            .def
            .annotation
            .as_ref()
            .map_or("Any".to_string(), extract_type);
        let description = ""; // You'd need to extract this from the docstring
        let default = "_required_".to_string(); // Since kw_defaults is not available, we'll assume all are required

        table.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            name, arg_type, description, default
        ));
    }

    table
}

fn format_returns_table(returns: &Option<Box<ast::Expr>>) -> String {
    let mut table = String::from("\n**Returns:**\n\n| Type | Description |\n| --- | --- |\n");

    if let Some(ret) = returns {
        let ret_type = extract_type(ret);
        let description = ""; // You'd need to extract this from the docstring
        table.push_str(&format!("| `{}` | {} |\n", ret_type, description));
    } else {
        table.push_str("| None | This function doesn't return a value. |\n");
    }

    table
}

fn format_function_doc(func_def: &ast::Stmt) -> String {
    if let ast::Stmt::FunctionDef(func_def) = func_def {
        let mut doc = String::new();

        // Clean the function name and add it to the documentation
        let clean_name = func_def.name.trim_matches('`');
        doc.push_str(&format!("### `{}`\n\n", clean_name));

        // Add docstring if available
        if let Some(ast::Stmt::Expr(expr)) = func_def.body.first() {
            if let ast::Expr::Constant(ast::ExprConstant {
                value: ast::Constant::Str(docstring),
                ..
            }) = &*expr.value
            {
                // Remove any leading/trailing whitespace and quotes from the docstring
                let cleaned_docstring = docstring.trim().trim_matches('"').trim_matches('\'');
                doc.push_str(&format!("{}\n\n", cleaned_docstring));
            }
        }

        // Add parameters table
        doc.push_str(&format_args_table(&func_def.args));

        // Add returns table
        doc.push_str(&format_returns_table(&func_def.returns));

        // Add prose description (extracted from docstring)
        doc.push_str("\n**Description:**\n\n");
        if let Some(ast::Stmt::Expr(expr)) = func_def.body.first() {
            if let ast::Expr::Constant(ast::ExprConstant {
                value: ast::Constant::Str(docstring),
                ..
            }) = &*expr.value
            {
                // Extract description from docstring (assuming it's after the first empty line)
                let description = docstring.splitn(2, "\n\n").nth(1).unwrap_or("");
                doc.push_str(&format!("{}\n", description.trim()));
            }
        }

        doc
    } else {
        String::new()
    }
}

fn reconstruct_function_def(func_def: &ast::StmtFunctionDef) -> String {
    let mut func_str = String::new();

    // Add decorators
    for decorator in &func_def.decorator_list {
        func_str.push_str(&format!(
            "@{}\n",
            extract_type(&Box::new(decorator.clone()))
        ));
    }

    // Function signature
    func_str.push_str(&format!("def {}(", func_def.name));

    // Arguments
    let args: Vec<String> = func_def
        .args
        .args
        .iter()
        .map(|arg| {
            let mut arg_str = arg.def.arg.to_string();
            if let Some(annotation) = &arg.def.annotation {
                arg_str.push_str(&format!(": {}", extract_type(annotation)));
            }
            arg_str
        })
        .collect();
    func_str.push_str(&args.join(", "));

    // Return annotation
    if let Some(returns) = &func_def.returns {
        func_str.push_str(&format!(" -> {}", extract_type(returns)));
    }

    func_str.push_str("):\n");

    // Docstring (if available)
    if let Some(ast::Stmt::Expr(expr)) = func_def.body.first() {
        if let ast::Expr::Constant(ast::ExprConstant {
            value: ast::Constant::Str(docstring),
            ..
        }) = &*expr.value
        {
            func_str.push_str(&format!(
                "    \"\"\"\n    {}\n    \"\"\"\n",
                docstring.trim()
            ));
        }
    }

    // Function body
    for stmt in &func_def.body {
        match stmt {
            ast::Stmt::Expr(expr) => {
                // Skip the docstring, as it's already handled
                if let ast::Expr::Constant(ast::ExprConstant {
                    value: ast::Constant::Str(_),
                    ..
                }) = &*expr.value
                {
                    continue;
                }
                func_str.push_str(&format!("    {}\n", extract_type(&expr.value)));
            }
            ast::Stmt::Pass(_) => func_str.push_str("    pass\n"),
            ast::Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    func_str.push_str(&format!("    return {}\n", extract_type(value)));
                } else {
                    func_str.push_str("    return\n");
                }
            }
            ast::Stmt::If(if_stmt) => {
                func_str.push_str(&format!("    if {}:\n", extract_type(&if_stmt.test)));
                for body_stmt in &if_stmt.body {
                    func_str.push_str(&format!("        {}\n", reconstruct_stmt(body_stmt)));
                }
                if !if_stmt.orelse.is_empty() {
                    func_str.push_str("    else:\n");
                    for else_stmt in &if_stmt.orelse {
                        func_str.push_str(&format!("        {}\n", reconstruct_stmt(else_stmt)));
                    }
                }
            }
            ast::Stmt::Assign(assign) => {
                let targets: Vec<String> = assign
                    .targets
                    .iter()
                    .map(|t| extract_type(&Box::new(t.clone())))
                    .collect();
                func_str.push_str(&format!(
                    "    {} = {}\n",
                    targets.join(", "),
                    extract_type(&assign.value)
                ));
            }
            ast::Stmt::AugAssign(aug_assign) => {
                func_str.push_str(&format!(
                    "    {} {:?}= {}\n",
                    extract_type(&aug_assign.target),
                    aug_assign.op,
                    extract_type(&aug_assign.value)
                ));
            }
            ast::Stmt::For(for_stmt) => {
                func_str.push_str(&format!(
                    "    for {} in {}:\n",
                    extract_type(&for_stmt.target),
                    extract_type(&for_stmt.iter)
                ));
                for body_stmt in &for_stmt.body {
                    func_str.push_str(&format!("        {}\n", reconstruct_stmt(body_stmt)));
                }
            }
            ast::Stmt::While(while_stmt) => {
                func_str.push_str(&format!("    while {}:\n", extract_type(&while_stmt.test)));
                for body_stmt in &while_stmt.body {
                    func_str.push_str(&format!("        {}\n", reconstruct_stmt(body_stmt)));
                }
            }
            ast::Stmt::Raise(raise) => {
                if let Some(exc) = &raise.exc {
                    func_str.push_str(&format!("    raise {}\n", extract_type(exc)));
                } else {
                    func_str.push_str("    raise\n");
                }
            }
            _ => func_str.push_str(&format!("    # Unhandled statement: {:?}\n", stmt)),
        }
    }

    func_str
}

fn reconstruct_stmt(stmt: &ast::Stmt) -> String {
    match stmt {
        ast::Stmt::Expr(expr) => extract_type(&expr.value),
        ast::Stmt::Pass(_) => "pass".to_string(),
        ast::Stmt::Return(ret) => {
            if let Some(value) = &ret.value {
                format!("return {}", extract_type(value))
            } else {
                "return".to_string()
            }
        }
        ast::Stmt::If(if_stmt) => {
            let mut if_str = format!("if {}:\n", extract_type(&if_stmt.test));
            for body_stmt in &if_stmt.body {
                if_str.push_str(&format!("    {}\n", reconstruct_stmt(body_stmt)));
            }
            if !if_stmt.orelse.is_empty() {
                if_str.push_str("else:\n");
                for else_stmt in &if_stmt.orelse {
                    if_str.push_str(&format!("    {}\n", reconstruct_stmt(else_stmt)));
                }
            }
            if_str
        }
        ast::Stmt::Assign(assign) => {
            let targets: Vec<String> = assign
                .targets
                .iter()
                .map(|t| extract_type(&Box::new(t.clone())))
                .collect();
            format!("{} = {}", targets.join(", "), extract_type(&assign.value))
        }
        ast::Stmt::AugAssign(aug_assign) => {
            format!(
                "{} {:?}= {}",
                extract_type(&aug_assign.target),
                aug_assign.op,
                extract_type(&aug_assign.value)
            )
        }
        ast::Stmt::Raise(raise) => {
            if let Some(exc) = &raise.exc {
                format!("raise {}", extract_type(exc))
            } else {
                "raise".to_string()
            }
        }
        _ => format!("# Unhandled statement: {:?}", stmt),
    }
}

fn main() {
    let args = Args::parse();

    // Read the contents of the Python file
    let code = fs::read_to_string(&args.file).expect("Failed to read the Python file");

    // Parse the Python code
    let ast = ast::Suite::parse(&code, "<string>").expect("Failed to parse the Python file");

    // Create the output directory if it doesn't exist
    fs::create_dir_all(&args.output_path).expect("Failed to create output directory");

    // Generate the output file path
    let input_path = PathBuf::from(&args.file);
    let file_name = input_path
        .file_stem()
        .expect("Invalid file name")
        .to_str()
        .expect("Invalid file name");
    let output_file = args.output_path.join(format!("{}.mdx", file_name));

    // Extract the docstrings and write to the Markdown file
    let mut markdown_content = String::new();

    // Add the module header
    markdown_content.push_str("---\n");
    markdown_content.push_str(&format!("title: {}\n", file_name));
    markdown_content.push_str("---\n\n");
    markdown_content.push_str(&format!("## `zenml.{}` `special`\n\n", file_name));

    // Add the module docstring if it exists
    if let Some(first_stmt) = ast.first() {
        if let ast::Stmt::Expr(expr) = first_stmt {
            if let ast::Expr::Constant(ast::ExprConstant {
                value: ast::Constant::Str(docstring),
                ..
            }) = &*expr.value
            {
                markdown_content.push_str(&format!("{}\n\n", docstring));
            }
        }
    }

    // Extract class and function docstrings
    for (_index, stmt) in ast.iter().enumerate() {
        if let ast::Stmt::ClassDef(class_def) = stmt {
            markdown_content.push_str(&format!("### `{}`\n", class_def.name));
            markdown_content.push_str(" ([Integration](/integrations-integration/#zenml.integrations.integration.Integration \"zenml.integrations.integration.Integration\"))\n\n");

            if let Some(first_stmt) = class_def.body.first() {
                if let ast::Stmt::Expr(expr) = first_stmt {
                    if let ast::Expr::Constant(ast::ExprConstant {
                        value: ast::Constant::Str(docstring),
                        ..
                    }) = &*expr.value
                    {
                        markdown_content.push_str(&format!("{}\n", docstring));
                    }
                }
            }

            markdown_content.push_str(&format!(
                "<Accordion\n  title=\"Source code in `zenml/{}/{}.py`\"\n>\n",
                file_name, file_name
            ));
            markdown_content.push_str("```py\n");
            // Reconstruct the class definition
            markdown_content.push_str(&format!("class {}:\n", class_def.name));
            for stmt in &class_def.body {
                if let ast::Stmt::FunctionDef(func_def) = stmt {
                    markdown_content.push_str(&reconstruct_function_def(func_def));
                }
            }
            markdown_content.push_str("```\n");
            markdown_content.push_str("</Accordion>\n\n");

            // Extract methods
            for stmt in &class_def.body {
                if let ast::Stmt::FunctionDef(func_def) = stmt {
                    markdown_content.push_str(&format!(
                        "#### `{}()` `{}`\n\n",
                        func_def.name,
                        if is_classmethod(&func_def.decorator_list) {
                            "classmethod"
                        } else {
                            ""
                        }
                    ));

                    // Add the arguments table
                    markdown_content.push_str(&format_args_table(&func_def.args));

                    if let Some(first_stmt) = func_def.body.first() {
                        if let ast::Stmt::Expr(expr) = first_stmt {
                            if let ast::Expr::Constant(ast::ExprConstant {
                                value: ast::Constant::Str(docstring),
                                ..
                            }) = &*expr.value
                            {
                                markdown_content.push_str(&format!("{}\n", docstring));
                            }
                        }
                    }

                    markdown_content.push_str(&format!(
                        "<Accordion\n  title=\"Source code in `zenml/{}/{}.py`\"\n\n>\n",
                        file_name, file_name
                    ));
                    markdown_content.push_str("```py\n");
                    markdown_content.push_str(&reconstruct_function_def(func_def));
                    markdown_content.push_str("```\n");
                    markdown_content.push_str("</Accordion>\n\n");

                    // Add the returns table
                    markdown_content.push_str(&format_returns_table(&func_def.returns));
                }
            }
        } else if let ast::Stmt::FunctionDef(_) = stmt {
            markdown_content.push_str(&format_function_doc(stmt));
        }
    }

    // Write the Markdown content to the file
    fs::write(&output_file, markdown_content).expect("Failed to write Markdown file");

    println!("Markdown file generated: {:?}", output_file);
}
