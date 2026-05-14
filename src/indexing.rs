use crate::{Chunk, Language};
use std::path::{Path, PathBuf};
use tracing::warn;
use tree_sitter::{Node, Parser, Tree};

mod languages;

pub use languages::LanguageConfig;

pub async fn parse_file_async(
    path: PathBuf,
    relative_path: String,
    language: Language,
    codebase_id: String,
) -> Vec<Chunk> {
    tokio::task::spawn_blocking(move || parse_file(&path, &relative_path, language, &codebase_id))
        .await
        .unwrap_or_else(|e| {
            warn!("Task join error during parse_file_async: {e}");
            Vec::new()
        })
}

pub fn parse_file(
    path: &Path,
    relative_path: &str,
    language: Language,
    codebase_id: &str,
) -> Vec<Chunk> {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to read file {}: {e}", path.display());
            return Vec::new();
        }
    };

    let config = LanguageConfig::for_language(language);
    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(&config.tree_sitter_language()) {
        warn!(
            "Failed to set language for {:?}: {e}. Skipping file: {}",
            language,
            path.display()
        );
        return Vec::new();
    }

    match parser.parse(&source, None) {
        Some(tree) => extract_chunks(
            &tree,
            &source,
            relative_path,
            language,
            &config,
            codebase_id,
        ),
        None => {
            warn!(
                "Tree-sitter parse failed for {}, falling back to line splitting",
                path.display()
            );
            fallback_line_chunks(&source, relative_path, language, codebase_id)
        }
    }
}

fn extract_chunks(
    tree: &Tree,
    source: &str,
    relative_path: &str,
    language: Language,
    config: &LanguageConfig,
    codebase_id: &str,
) -> Vec<Chunk> {
    let root = tree.root_node();
    let mut chunks = Vec::new();
    let mut cursor = root.walk();
    let ctx = ExtractionContext {
        source,
        relative_path,
        language,
        config,
        codebase_id,
    };

    collect_chunks(&mut cursor, &ctx, &mut chunks, None);

    chunks
}

struct ExtractionContext<'a> {
    source: &'a str,
    relative_path: &'a str,
    language: Language,
    config: &'a LanguageConfig,
    codebase_id: &'a str,
}

fn collect_chunks<'a>(
    cursor: &mut tree_sitter::TreeCursor<'a>,
    ctx: &ExtractionContext<'a>,
    chunks: &mut Vec<Chunk>,
    parent_context: Option<ParentContext>,
) {
    let node = cursor.node();
    let node_type = node.kind();

    if ctx.config.is_target_node(node_type) {
        if let Some(chunk) = build_chunk(node, ctx, &parent_context) {
            chunks.push(chunk);
        }
    }

    let child_context = build_child_context(node, ctx.source, ctx.config);
    let next_parent_context = child_context.or(parent_context);

    if cursor.goto_first_child() {
        loop {
            collect_chunks(cursor, ctx, chunks, next_parent_context.clone());
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

#[derive(Debug, Clone)]
struct ParentContext {
    header: String,
    node_type: String,
}

fn build_chunk(
    node: Node,
    ctx: &ExtractionContext<'_>,
    parent_context: &Option<ParentContext>,
) -> Option<Chunk> {
    let content = chunk_content(node, ctx.source);
    if content.trim().is_empty() {
        return None;
    }
    if ctx.language == Language::Markdown
        && node.kind() == "section"
        && content.trim_start().starts_with("# ")
    {
        return None;
    }

    let name = extract_name(node, ctx.source)?;
    let node_type = normalize_node_type(node.kind(), &content);
    let signature = extract_signature(node, ctx.source);
    let start_line = node.start_position().row;
    let end_line = node.end_position().row;

    let full_content = match parent_context {
        Some(ctx) => {
            if node_type == ctx.node_type {
                content.clone()
            } else {
                format!("{}\n{}", ctx.header, content)
            }
        }
        None => content.clone(),
    };

    let parent_ctx_str = parent_context.as_ref().map(|ctx| ctx.header.clone());

    let chunk_id = Chunk::generate_id(ctx.codebase_id, ctx.relative_path, &name, start_line);

    Some(Chunk {
        chunk_id,
        codebase_id: ctx.codebase_id.to_string(),
        filepath: ctx.relative_path.to_string(),
        language: ctx.language,
        node_type,
        name,
        signature,
        content: full_content,
        parent_context: parent_ctx_str,
        start_line,
        end_line,
    })
}

fn build_child_context(node: Node, source: &str, config: &LanguageConfig) -> Option<ParentContext> {
    let node_type = node.kind();
    if !config.is_container_node(node_type) {
        return None;
    }

    let header = extract_container_header(node, source)?;
    Some(ParentContext {
        header,
        node_type: normalize_node_type(node_type, &node_text(node, source)),
    })
}

fn extract_container_header(node: Node, source: &str) -> Option<String> {
    let start = node.start_position().row;
    let end = node.end_position().row;

    if end - start <= 3 {
        return Some(node_text(node, source));
    }

    let lines: Vec<&str> = source.lines().take(start + 3).skip(start).collect();
    Some(lines.join("\n"))
}

fn extract_name(node: Node, source: &str) -> Option<String> {
    if let Some(name) = extract_special_name(node, source) {
        return Some(name);
    }

    if node.kind() == "type_declaration" {
        if let Some(name) = type_declaration_name(&node_text(node, source)) {
            return Some(name);
        }
    }

    for field_name in ["name", "type", "heading_content", "declarator"] {
        if let Some(child) = node.child_by_field_name(field_name) {
            let text = clean_name(&node_text(child, source));
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "type_identifier" | "field_identifier" | "property_identifier" => {
                return Some(clean_name(&node_text(child, source)));
            }
            "name" => {
                return Some(clean_name(&node_text(child, source)));
            }
            _ => {}
        }
    }
    Some(format!("{}_{}", node.kind(), node.start_position().row))
}

fn extract_special_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "section" => extract_markdown_section_name(node, source),
        "atx_heading" | "setext_heading" => Some(markdown_heading_text(&node_text(node, source))),
        "fenced_code_block" | "indented_code_block" => Some(markdown_code_block_name(node, source)),
        "element" => html_element_name(node, source),
        "script_element" => Some(html_script_or_style_name(node, source, "script")),
        "style_element" => Some(html_script_or_style_name(node, source, "style")),
        "implementation_definition" => objc_named_declaration(node, source, "@implementation"),
        "class_interface" | "class_implementation" => first_identifier_name(node, source),
        "class_declaration"
            if node_text(node, source)
                .trim_start()
                .starts_with("@interface") =>
        {
            objc_named_declaration(node, source, "@interface")
        }
        "protocol_declaration"
            if node_text(node, source)
                .trim_start()
                .starts_with("@protocol") =>
        {
            objc_named_declaration(node, source, "@protocol")
        }
        _ => None,
    }
}

fn type_declaration_name(text: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "type" {
        return None;
    }
    parts.next().map(ToString::to_string)
}

fn first_identifier_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(child.kind(), "identifier" | "type_identifier") {
            return Some(clean_name(&node_text(child, source)));
        }
    }
    None
}

fn extract_signature(node: Node, source: &str) -> String {
    let kind = node.kind();
    let text = chunk_content(node, source);

    if matches!(kind, "class_interface" | "class_implementation") {
        return text.lines().next().unwrap_or("").trim().to_string();
    }

    if matches!(
        kind,
        "struct_item"
            | "struct_declaration"
            | "enum_item"
            | "enum_declaration"
            | "trait_item"
            | "interface_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "class_definition"
            | "class_interface"
            | "class_implementation"
            | "class_declaration"
            | "protocol_declaration"
            | "implementation_definition"
            | "extension_declaration"
            | "type_alias"
            | "type_alias_declaration"
            | "typealias_declaration"
            | "type_declaration"
    ) {
        let first_line = first_signature_line(&text);
        if let Some(pos) = first_line.find('{') {
            return first_line[..pos].trim().to_string();
        }
        return first_line.to_string();
    }

    if matches!(
        kind,
        "function_item"
            | "function_definition"
            | "function_declaration"
            | "method_declaration"
            | "method_definition"
            | "constructor_declaration"
            | "init_declaration"
            | "property_declaration"
            | "subscript_declaration"
    ) {
        let first_line = first_signature_line(&text);
        if let Some(stripped) = first_line.strip_suffix('{') {
            return stripped.trim().to_string();
        }
        if first_line.ends_with("->") || first_line.ends_with('=') {
            let lines: Vec<&str> = text.lines().take(3).collect();
            for line in &lines {
                let l = line.trim();
                if let Some(stripped) = l.strip_suffix('{') {
                    return stripped.trim().to_string();
                }
            }
        }
        return first_line.to_string();
    }

    let first_line = first_signature_line(&text);
    if first_line.len() > 200 {
        first_line[..200].to_string()
    } else {
        first_line.to_string()
    }
}

fn first_signature_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('@'))
        .unwrap_or("")
}

fn node_text(node: Node, source: &str) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    source[start..end].to_string()
}

fn chunk_content(node: Node, source: &str) -> String {
    if node
        .parent()
        .is_some_and(|parent| parent.kind() == "decorated_definition")
    {
        return node_text(node.parent().unwrap(), source);
    }
    node_text(node, source)
}

fn normalize_node_type(kind: &str, text: &str) -> String {
    match kind {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_declaration"
        | "method_definition" => "function".to_string(),
        "struct_item" | "struct_declaration" => "struct".to_string(),
        "class_declaration" if text.trim_start().starts_with("struct ") => "struct".to_string(),
        "class_declaration" if text.trim_start().starts_with("enum ") => "enum".to_string(),
        "type_declaration" if text.contains(" interface") || text.contains("interface {") => {
            "interface".to_string()
        }
        "type_declaration" => "type".to_string(),
        "enum_item" | "enum_declaration" => "enum".to_string(),
        "impl_item" => "impl".to_string(),
        "trait_item" => "trait".to_string(),
        "class_definition" => "class".to_string(),
        "interface_declaration" => "interface".to_string(),
        "protocol_declaration" => "protocol".to_string(),
        "class_interface" => "class".to_string(),
        "class_implementation" => "implementation".to_string(),
        "implementation_definition" => "implementation".to_string(),
        "class_declaration" => "class".to_string(),
        "constructor_declaration" => "constructor".to_string(),
        "record_declaration" => "record".to_string(),
        "annotation_type_declaration" => "annotation".to_string(),
        "extension_declaration" => "extension".to_string(),
        "init_declaration" => "constructor".to_string(),
        "property_declaration" => "property".to_string(),
        "subscript_declaration" => "subscript".to_string(),
        "type_alias" | "type_alias_declaration" | "typealias_declaration" => {
            "type_alias".to_string()
        }
        "const_item" | "const_declaration" => "const".to_string(),
        "atx_heading" | "setext_heading" => "heading".to_string(),
        "fenced_code_block" | "indented_code_block" => "code_block".to_string(),
        "script_element" => "script".to_string(),
        "style_element" => "style".to_string(),
        _ => kind.to_string(),
    }
}

fn clean_name(text: &str) -> String {
    text.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn extract_markdown_section_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let heading = node
        .children(&mut cursor)
        .find(|child| matches!(child.kind(), "atx_heading" | "setext_heading"))
        .map(|heading| markdown_heading_text(&node_text(heading, source)));
    heading
}

fn markdown_heading_text(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('#')
        .trim()
        .trim_end_matches('#')
        .trim()
        .to_string()
}

fn markdown_code_block_name(node: Node, source: &str) -> String {
    let first_line = node_text(node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let info = first_line
        .trim_start_matches('`')
        .trim_start_matches('~')
        .trim();
    if info.is_empty() {
        "code block".to_string()
    } else {
        format!("{info} code block")
    }
}

fn html_element_name(node: Node, source: &str) -> Option<String> {
    let start_tag = first_child_of_kind(node, "start_tag")?;
    let tag_name = first_child_of_kind(start_tag, "tag_name")
        .map(|tag| node_text(tag, source))
        .filter(|tag| !tag.trim().is_empty())?;
    let mut name = tag_name.trim().to_string();

    if let Some(id) = html_attribute_value(start_tag, source, "id") {
        name.push('#');
        name.push_str(&id);
    } else if let Some(class) = html_attribute_value(start_tag, source, "class") {
        name.push('.');
        name.push_str(&class.replace(' ', "."));
    }

    Some(name)
}

fn html_script_or_style_name(node: Node, source: &str, tag: &str) -> String {
    let Some(start_tag) = first_child_of_kind(node, "start_tag") else {
        return tag.to_string();
    };
    if let Some(module_type) = html_attribute_value(start_tag, source, "type") {
        format!("{tag}[type={module_type}]")
    } else {
        tag.to_string()
    }
}

fn html_attribute_value(node: Node, source: &str, name: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "attribute" {
            continue;
        }
        let text = node_text(child, source);
        let Some((attr_name, attr_value)) = text.split_once('=') else {
            continue;
        };
        if attr_name.trim().eq_ignore_ascii_case(name) {
            return Some(clean_name(attr_value));
        }
    }
    None
}

fn objc_named_declaration(node: Node, source: &str, marker: &str) -> Option<String> {
    let text = node_text(node, source);
    let after_marker = text.trim_start().strip_prefix(marker)?.trim_start();
    after_marker
        .split(|c: char| c.is_whitespace() || c == ':' || c == '(' || c == '<')
        .find(|part| !part.is_empty())
        .map(ToString::to_string)
}

fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let child = node
        .children(&mut cursor)
        .find(|child| child.kind() == kind);
    child
}

fn fallback_line_chunks(
    source: &str,
    relative_path: &str,
    language: Language,
    codebase_id: &str,
) -> Vec<Chunk> {
    let lines: Vec<&str> = source.lines().collect();
    let chunk_size = 30;
    let mut chunks = Vec::new();

    for (i, chunk_lines) in lines.chunks(chunk_size).enumerate() {
        let start_line = i * chunk_size;
        let end_line = start_line + chunk_lines.len().saturating_sub(1);
        let content = chunk_lines.join("\n");
        let name = format!("lines_{}_{}", start_line, end_line);
        let chunk_id = Chunk::generate_id(codebase_id, relative_path, &name, start_line);

        chunks.push(Chunk {
            chunk_id,
            codebase_id: codebase_id.to_string(),
            filepath: relative_path.to_string(),
            language,
            node_type: "text_block".to_string(),
            name: name.clone(),
            signature: format!("lines {}-{}", start_line, end_line),
            content,
            parent_context: None,
            start_line,
            end_line,
        });
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks_for(source: &str, relative_path: &str, language: Language) -> Vec<Chunk> {
        let config = LanguageConfig::for_language(language);
        let mut parser = Parser::new();
        parser.set_language(&config.tree_sitter_language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        extract_chunks(
            &tree,
            source,
            relative_path,
            language,
            &config,
            "test-codebase",
        )
    }

    fn find_chunk<'a>(chunks: &'a [Chunk], name: &str) -> &'a Chunk {
        chunks
            .iter()
            .find(|chunk| chunk.name == name)
            .unwrap_or_else(|| panic!("missing chunk named {name}; got {chunks:#?}"))
    }

    #[test]
    fn test_parse_rust_function() {
        let source = r#"
fn hello_world() -> String {
    "hello".to_string()
}
"#;
        let config = LanguageConfig::for_language(Language::Rust);
        let mut parser = Parser::new();
        parser.set_language(&config.tree_sitter_language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let chunks = extract_chunks(
            &tree,
            source,
            "test.rs",
            Language::Rust,
            &config,
            "test-codebase",
        );
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].node_type, "function");
        assert_eq!(chunks[0].name, "hello_world");
    }

    #[test]
    fn test_parse_python_class_function_and_decorator_context() {
        let source = r#"
@dataclass
class UserService:
    """Coordinates user access."""

    @classmethod
    def from_config(cls, config: dict[str, str]) -> "UserService":
        return cls()

async def fetch_user(user_id: str) -> dict:
    return {"id": user_id}
"#;

        let chunks = chunks_for(source, "service.py", Language::Python);

        let class_chunk = find_chunk(&chunks, "UserService");
        assert_eq!(class_chunk.node_type, "class");
        assert!(class_chunk.signature.contains("class UserService"));

        let method = find_chunk(&chunks, "from_config");
        assert_eq!(method.node_type, "function");
        assert!(method
            .parent_context
            .as_deref()
            .unwrap()
            .contains("class UserService"));
        assert!(method.content.contains("@classmethod"));

        let async_fn = find_chunk(&chunks, "fetch_user");
        assert_eq!(async_fn.node_type, "function");
        assert!(async_fn.signature.starts_with("async def fetch_user"));
    }

    #[test]
    fn test_parse_swift_type_function_property_and_extension_context() {
        let source = r#"
struct ProfileViewModel {
    let title: String

    func render() -> String {
        title
    }
}

extension ProfileViewModel: CustomStringConvertible {
    var description: String {
        render()
    }
}
"#;

        let chunks = chunks_for(source, "ProfileViewModel.swift", Language::Swift);

        let type_chunk = find_chunk(&chunks, "ProfileViewModel");
        assert_eq!(type_chunk.node_type, "struct");

        let render = find_chunk(&chunks, "render");
        assert_eq!(render.node_type, "function");
        assert!(render
            .parent_context
            .as_deref()
            .unwrap()
            .contains("struct ProfileViewModel"));

        let description = find_chunk(&chunks, "description");
        assert_eq!(description.node_type, "property");
        assert!(description
            .parent_context
            .as_deref()
            .unwrap()
            .contains("extension ProfileViewModel"));
    }

    #[test]
    fn test_parse_objective_c_interface_implementation_and_method_context() {
        let source = r#"
@interface UserController : NSObject
- (NSString *)displayNameForUser:(User *)user;
@end

@implementation UserController
- (NSString *)displayNameForUser:(User *)user {
    return user.name;
}
@end
"#;

        let chunks = chunks_for(source, "UserController.m", Language::ObjectiveC);

        let interface = find_chunk(&chunks, "UserController");
        assert_eq!(interface.node_type, "class");
        assert!(interface.signature.contains("@interface UserController"));

        let method = chunks
            .iter()
            .find(|chunk| {
                chunk.node_type == "function"
                    && chunk.signature.contains("displayName")
                    && chunk
                        .parent_context
                        .as_deref()
                        .is_some_and(|ctx| ctx.contains("@implementation UserController"))
            })
            .unwrap_or_else(|| panic!("missing Objective-C method; got {chunks:#?}"));
        assert!(method
            .parent_context
            .as_deref()
            .unwrap()
            .contains("@implementation UserController"));
    }

    #[test]
    fn test_parse_markdown_headings_and_fenced_code_blocks() {
        let source = r#"# KT Guide

Intro text.

## Syncing

```rust
fn sync_repo() {}
```

### Troubleshooting

Check Redis.
"#;

        let chunks = chunks_for(source, "README.md", Language::Markdown);

        let guide = find_chunk(&chunks, "KT Guide");
        assert_eq!(guide.node_type, "heading");
        assert!(guide.signature.contains("# KT Guide"));

        let syncing = find_chunk(&chunks, "Syncing");
        assert_eq!(syncing.node_type, "section");
        assert!(syncing.content.contains("fn sync_repo"));

        let code_block = chunks
            .iter()
            .find(|chunk| chunk.node_type == "code_block")
            .unwrap_or_else(|| panic!("missing markdown code block; got {chunks:#?}"));
        assert_eq!(code_block.name, "rust code block");
    }

    #[test]
    fn test_parse_html_semantic_elements_scripts_and_styles() {
        let source = r#"<!doctype html>
<html>
  <head>
    <title>KT Dashboard</title>
    <style>.status { color: green; }</style>
  </head>
  <body>
    <main id="dashboard">
      <section class="summary">
        <h1>Sync Status</h1>
      </section>
      <script type="module">console.log("ready");</script>
    </main>
  </body>
</html>
"#;

        let chunks = chunks_for(source, "index.html", Language::Html);

        let dashboard = find_chunk(&chunks, "main#dashboard");
        assert_eq!(dashboard.node_type, "element");
        assert!(dashboard.content.contains("Sync Status"));

        let style = chunks
            .iter()
            .find(|chunk| chunk.node_type == "style")
            .unwrap_or_else(|| panic!("missing style element; got {chunks:#?}"));
        assert_eq!(style.name, "style");

        let script = chunks
            .iter()
            .find(|chunk| chunk.node_type == "script")
            .unwrap_or_else(|| panic!("missing script element; got {chunks:#?}"));
        assert_eq!(script.name, "script[type=module]");
    }

    #[test]
    fn test_parse_rust_impl_with_parent_context() {
        let source = r#"
struct Foo {
    x: i32,
}

impl Foo {
    pub fn get_x(&self) -> i32 {
        self.x
    }
}
"#;
        let config = LanguageConfig::for_language(Language::Rust);
        let mut parser = Parser::new();
        parser.set_language(&config.tree_sitter_language()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let chunks = extract_chunks(
            &tree,
            source,
            "test.rs",
            Language::Rust,
            &config,
            "test-codebase",
        );
        assert!(chunks.len() >= 2);

        let impl_fn = chunks
            .iter()
            .find(|c| c.name == "get_x")
            .expect("should find get_x");
        assert!(impl_fn.content.contains("impl"));
        assert_eq!(impl_fn.node_type, "function");
        assert!(chunks.iter().all(|c| c.node_type != "impl"));
    }

    #[test]
    fn test_parse_go_interface_method_and_receiver_context() {
        let source = r#"
type Repository interface {
    Find(id string) (User, error)
}

type MemoryRepository struct{}

func (r *MemoryRepository) Find(id string) (User, error) {
    return User{}, nil
}
"#;

        let chunks = chunks_for(source, "repository.go", Language::Go);

        let interface = find_chunk(&chunks, "Repository");
        assert_eq!(interface.node_type, "interface");

        let method = find_chunk(&chunks, "Find");
        assert_eq!(method.node_type, "function");
        assert!(method.signature.contains("MemoryRepository"));
        assert_eq!(method.parent_context, None);
    }

    #[test]
    fn test_parse_java_annotation_record_and_method_context() {
        let source = r#"
@Deprecated
public record UserDto(String id) {
    public String displayName() {
        return id;
    }
}
"#;

        let chunks = chunks_for(source, "UserDto.java", Language::Java);

        let record = find_chunk(&chunks, "UserDto");
        assert_eq!(record.node_type, "record");
        assert!(record.content.contains("@Deprecated"));

        let method = find_chunk(&chunks, "displayName");
        assert_eq!(method.node_type, "function");
        assert!(method
            .parent_context
            .as_deref()
            .unwrap()
            .contains("record UserDto"));
    }

    #[test]
    fn test_fallback_line_chunks() {
        let source = "line1\nline2\nline3\nline4\n";
        let chunks = fallback_line_chunks(source, "test.rs", Language::Rust, "test-codebase");
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].node_type, "text_block");
    }

    #[tokio::test]
    async fn test_parse_file_async() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let chunks = parse_file_async(
            file_path,
            "test.rs".to_string(),
            Language::Rust,
            "test-codebase".to_string(),
        )
        .await;

        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].name, "main");
    }

    #[test]
    fn test_parse_file_handles_read_errors() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("nonexistent.rs");

        let chunks = parse_file(
            &file_path,
            "nonexistent.rs",
            Language::Rust,
            "test-codebase",
        );

        assert!(
            chunks.is_empty(),
            "Should return empty Vec on file read error"
        );
    }
}
