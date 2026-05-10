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
                    "impl_item",
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
                container_node_types: &["type_declaration", "source_file"],
                ts_language: tree_sitter_go::LANGUAGE,
            },
            Language::Java => Self {
                language: Language::Java,
                target_node_types: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                    "method_declaration",
                    "constructor_declaration",
                ],
                container_node_types: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                ],
                ts_language: tree_sitter_java::LANGUAGE,
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
