use crate::Language;
use tree_sitter::Language as TsLanguage;
use tree_sitter_language::LanguageFn;

#[derive(Clone, Copy)]
pub struct LanguageConfig {
    pub language: Language,
    pub target_node_types: &'static [&'static str],
    pub container_node_types: &'static [&'static str],
    ts_language: LanguageFn,
}

impl std::fmt::Debug for LanguageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageConfig")
            .field("language", &self.language)
            .field("target_node_types", &self.target_node_types)
            .field("container_node_types", &self.container_node_types)
            .finish_non_exhaustive()
    }
}

impl LanguageConfig {
    pub fn for_language(lang: Language) -> Self {
        match lang {
            Language::Rust => Self {
                language: Language::Rust,
                target_node_types: &[
                    "function_item",
                    "struct_item",
                    "enum_item",
                    "trait_item",
                    "type_alias",
                    "const_item",
                ],
                container_node_types: &["impl_item", "trait_item", "mod_item"],
                ts_language: tree_sitter_rust::LANGUAGE,
            },
            Language::Go => Self {
                language: Language::Go,
                target_node_types: &[
                    "function_declaration",
                    "method_declaration",
                    "type_declaration",
                ],
                container_node_types: &["type_declaration"],
                ts_language: tree_sitter_go::LANGUAGE,
            },
            Language::Java => Self {
                language: Language::Java,
                target_node_types: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                    "record_declaration",
                    "annotation_type_declaration",
                    "method_declaration",
                    "constructor_declaration",
                ],
                container_node_types: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                    "record_declaration",
                    "annotation_type_declaration",
                ],
                ts_language: tree_sitter_java::LANGUAGE,
            },
            Language::Python => Self {
                language: Language::Python,
                target_node_types: &["class_definition", "function_definition"],
                container_node_types: &["class_definition"],
                ts_language: tree_sitter_python::LANGUAGE,
            },
            Language::Swift => Self {
                language: Language::Swift,
                target_node_types: &[
                    "class_declaration",
                    "struct_declaration",
                    "enum_declaration",
                    "protocol_declaration",
                    "extension_declaration",
                    "function_declaration",
                    "init_declaration",
                    "property_declaration",
                    "subscript_declaration",
                    "typealias_declaration",
                ],
                container_node_types: &[
                    "class_declaration",
                    "struct_declaration",
                    "enum_declaration",
                    "protocol_declaration",
                    "extension_declaration",
                ],
                ts_language: tree_sitter_swift::LANGUAGE,
            },
            Language::ObjectiveC => Self {
                language: Language::ObjectiveC,
                target_node_types: &[
                    "class_interface",
                    "class_implementation",
                    "class_declaration",
                    "protocol_declaration",
                    "implementation_definition",
                    "method_declaration",
                    "method_definition",
                    "property_declaration",
                    "function_definition",
                    "struct_declaration",
                    "enum_specifier",
                ],
                container_node_types: &[
                    "class_interface",
                    "class_implementation",
                    "class_declaration",
                    "protocol_declaration",
                ],
                ts_language: tree_sitter_objc::LANGUAGE,
            },
            Language::Markdown => Self {
                language: Language::Markdown,
                target_node_types: &[
                    "section",
                    "atx_heading",
                    "setext_heading",
                    "fenced_code_block",
                    "indented_code_block",
                    "html_block",
                    "pipe_table",
                    "list",
                    "block_quote",
                ],
                container_node_types: &["section", "block_quote", "list_item"],
                ts_language: tree_sitter_md_025::LANGUAGE,
            },
            Language::Html => Self {
                language: Language::Html,
                target_node_types: &[
                    "element",
                    "script_element",
                    "style_element",
                    "doctype",
                    "comment",
                ],
                container_node_types: &["element"],
                ts_language: tree_sitter_html::LANGUAGE,
            },
        }
    }

    pub fn tree_sitter_language(&self) -> TsLanguage {
        TsLanguage::from(self.ts_language)
    }

    pub fn is_target_node(&self, node_type: &str) -> bool {
        self.target_node_types.contains(&node_type)
    }

    pub fn is_container_node(&self, node_type: &str) -> bool {
        self.container_node_types.contains(&node_type)
    }
}
