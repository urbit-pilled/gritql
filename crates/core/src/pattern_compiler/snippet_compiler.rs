use super::{
    back_tick_compiler::{BackTickCompiler, RawBackTickCompiler},
    compiler::SnippetCompilationContext,
    pattern_compiler::PatternCompiler,
    NodeCompiler,
};
use crate::{marzano_code_snippet::MarzanoCodeSnippet, problem::MarzanoQueryContext};
use crate::{pattern_compiler::compiler::NodeCompilationContext, split_snippet::split_snippet};
use anyhow::{anyhow, bail, Result};
use grit_pattern_matcher::pattern::{DynamicPattern, DynamicSnippet, DynamicSnippetPart, Pattern};
use grit_util::{AstNode, ByteRange, Language};
use marzano_language::{
    language::{nodes_from_indices, MarzanoLanguage, SortId},
    target_language::TargetLanguage,
};
use marzano_util::node_with_source::NodeWithSource;

pub(crate) struct CodeSnippetCompiler;

impl NodeCompiler for CodeSnippetCompiler {
    type TargetPattern = Pattern<MarzanoQueryContext>;

    fn from_node_with_rhs(
        node: &NodeWithSource,
        context: &mut NodeCompilationContext,
        is_rhs: bool,
    ) -> Result<Self::TargetPattern> {
        let snippet = node
            .child_by_field_name("source")
            .ok_or_else(|| anyhow!("missing content of codeSnippet"))?;
        match snippet.node.kind().as_ref() {
            "backtickSnippet" => BackTickCompiler::from_node_with_rhs(&snippet, context, is_rhs),
            "rawBacktickSnippet" => {
                RawBackTickCompiler::from_node_with_rhs(&snippet, context, is_rhs)
            }
            "languageSpecificSnippet" => {
                LanguageSpecificSnippetCompiler::from_node_with_rhs(&snippet, context, is_rhs)
            }
            _ => bail!("invalid code snippet kind: {}", snippet.node.kind()),
        }
    }
}

pub(crate) struct LanguageSpecificSnippetCompiler;

impl NodeCompiler for LanguageSpecificSnippetCompiler {
    type TargetPattern = Pattern<MarzanoQueryContext>;

    fn from_node_with_rhs(
        node: &NodeWithSource,
        context: &mut NodeCompilationContext,
        is_rhs: bool,
    ) -> Result<Self::TargetPattern> {
        let lang_node = node
            .child_by_field_name("language")
            .ok_or_else(|| anyhow!("missing language of languageSpecificSnippet"))?;
        let lang_name = lang_node.text()?.trim().to_string();
        let _snippet_lang = TargetLanguage::from_string(&lang_name, None)
            .ok_or_else(|| anyhow!("invalid language: {lang_name}"))?;
        let snippet_node = node
            .child_by_field_name("snippet")
            .ok_or_else(|| anyhow!("missing snippet of languageSpecificSnippet"))?;
        let source = snippet_node.text()?.to_string();
        let mut range = node.range();
        range.adjust_columns(1, -1);
        let content = source
            .strip_prefix('"')
            .ok_or_else(|| anyhow!("Unable to extract content from raw snippet: {source}"))?
            .strip_suffix('"')
            .ok_or_else(|| anyhow!("Unable to extract content from raw snippet: {source}"))?;

        parse_snippet_content(content, range.into(), context, is_rhs)
    }
}

pub(crate) fn dynamic_snippet_from_source(
    raw_source: &str,
    source_range: ByteRange,
    context: &mut dyn SnippetCompilationContext,
) -> Result<DynamicSnippet> {
    println!("\n=== Starting dynamic_snippet_from_source ===");
    println!("Raw source: {}", raw_source);
    println!("Source range: {:?}", source_range);

    // Process escape sequences
    let source_string = raw_source
        .replace("\\n", "\n")
        .replace("\\$", "$")
        .replace("\\^", "^")
        .replace("\\`", "`")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");
    println!("After escape processing: {}", source_string);

    let source = source_string.as_str();

    // Find all metavariables in the source
    let metavariables = split_snippet(source, context.get_lang());
    println!("Found {} metavariables:", metavariables.len());
    for (range, var) in &metavariables {
        println!("  - {} at range {:?}", var, range);
    }

    // Create parts alternating between string literals and variables
    let mut parts = Vec::with_capacity(2 * metavariables.len() + 1);
    let mut last = 0;

    // Process metavariables in reverse order to maintain correct positions
    println!("\nProcessing parts:");
    for (byte_range, var) in metavariables.into_iter().rev() {
        // Add text before the variable
        let prefix = &source[last..byte_range.start];
        println!("Adding string part: {:?}", prefix);
        parts.push(DynamicSnippetPart::String(prefix.to_string()));

        // Calculate variable range in original source
        let range = ByteRange::new(
            source_range.start + byte_range.start,
            source_range.start + byte_range.start + var.len(),
        );
        println!("Processing variable {} at range {:?}", var, range);

        // Register the variable and add it as a part
        let part = context.register_snippet_variable(&var, Some(range))?;
        println!("Added variable part: {:?}", part);
        parts.push(part);

        last = byte_range.end;
    }

    // Add remaining text after last variable
    let remaining = &source[last..];
    println!("Adding final string part: {:?}", remaining);
    parts.push(DynamicSnippetPart::String(remaining.to_string()));

    println!("\nFinal DynamicSnippet has {} parts", parts.len());
    println!("=== Completed dynamic_snippet_from_source ===\n");
    let snippet = DynamicSnippet { parts };
    println!("{:#?}", &snippet);
    Ok(snippet)
}

pub(crate) fn parse_snippet_content(
    source: &str,
    range: ByteRange,
    context: &mut dyn SnippetCompilationContext,
    is_rhs: bool,
) -> Result<Pattern<MarzanoQueryContext>> {
    println!("\n=== Starting parse_snippet_content ===");
    println!("Source: {}", source);
    println!("Range: {:?}", range);
    println!("Is RHS: {}", is_rhs);

    // Check for bracketed metavariables like ${name}
    let has_bracketed_vars = context
        .get_lang()
        .metavariable_bracket_regex()
        .is_match(source);
    println!("Has bracketed variables: {}", has_bracketed_vars);

    if has_bracketed_vars {
        println!("Processing bracketed metavariables pattern");
        if is_rhs {
            println!("-> Creating dynamic pattern for RHS");
            return Ok(Pattern::Dynamic(
                dynamic_snippet_from_source(source, range, context).map(DynamicPattern::Snippet)?,
            ));
        } else {
            println!("-> Error: bracketed vars not allowed on LHS");
            bail!("bracketed metavariables are only allowed on the rhs of a snippet");
        }
    }

    // Check for single metavariable patterns
    let is_exact_variable = context
        .get_lang()
        .exact_variable_regex()
        .is_match(source.trim());
    println!("Is exact variable match: {}", is_exact_variable);

    if is_exact_variable {
        println!("Processing exact variable pattern: {}", source.trim());
        match source.trim() {
            "$_" => {
                println!("-> Returning Underscore pattern");
                return Ok(Pattern::Underscore);
            }
            "^_" => {
                println!("-> Returning Underscore pattern");
                return Ok(Pattern::Underscore);
            }
            name => {
                println!("-> Creating Variable pattern for: {}", name);
                let var = context.register_variable(name, Some(range))?;
                return Ok(Pattern::Variable(var));
            }
        }
    }

    // Parse regular code snippet
    println!("Parsing snippet as code...");
    let snippet_trees = context.get_lang().parse_snippet_contexts(source);
    //print snippet trees
    println!("snippet_trees: {:#?}", snippet_trees);

    let snippet_nodes = nodes_from_indices(&snippet_trees);
    println!("Number of parsed nodes: {}", snippet_nodes.len());

    if snippet_nodes.is_empty() {
        println!("No AST nodes found - creating dynamic snippet pattern");
        return Ok(Pattern::Dynamic(
            dynamic_snippet_from_source(source, range, context).map(DynamicPattern::Snippet)?,
        ));
    }

    println!("Processing {} AST nodes", snippet_nodes.len());
    let snippet_patterns: Vec<(SortId, Pattern<MarzanoQueryContext>)> = snippet_nodes
        .into_iter()
        .map(|node| {
            println!("Processing node kind: {}", node.node.kind());
            Ok((
                node.node.kind_id(),
                PatternCompiler::from_snippet_node(node, range, context, is_rhs)?,
            ))
        })
        .collect::<Result<Vec<(SortId, Pattern<MarzanoQueryContext>)>>>()?;

    println!("Creating dynamic snippet");
    let dynamic_snippet = dynamic_snippet_from_source(source, range, context)
        .map_or(None, |s| Some(DynamicPattern::Snippet(s)));

    println!(
        "-> Returning CodeSnippet pattern with {} patterns",
        snippet_patterns.len()
    );
    println!("=== Completed parse_snippet_content ===\n");

    Ok(Pattern::CodeSnippet(MarzanoCodeSnippet::new(
        snippet_patterns,
        dynamic_snippet,
        source,
    )))
}
