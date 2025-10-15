use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use git2::{Repository, BranchType, Oid};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    collections::HashMap,
    io,
    path::Path,
};

#[derive(Debug, Clone)]
struct GitCommit {
    id: String,
    short_id: String,
    message: String,
    author: String,
    timestamp: DateTime<Utc>,
    parents: Vec<String>,
    refs: Vec<String>, // Branch and tag references
}

#[derive(Debug, Clone)]
struct GitBranch {
    name: String,
    commit_id: String,
    is_head: bool,
    is_remote: bool,
}

#[derive(Debug, Clone)]
struct GraphLine {
    commit_id: String,
    graph_text: String,
    commit_text: String,
    refs_text: String,
}

struct App {
    repository: Repository,
    branches: Vec<GitBranch>,
    commits: HashMap<String, GitCommit>,
    graph_lines: Vec<GraphLine>,
    selected_branch: usize,
    selected_commit: usize,
    branch_list_state: ListState,
    commit_list_state: ListState,
    show_logs: bool,
    current_branch_filter: Option<String>,
    loading: bool,
    error_message: Option<String>,
    scroll_offset: u16, // For scrolling commit details
    // Diff viewing
    current_diff: Option<String>,
    show_diff: bool,
    diff_scroll_offset: u16,
    // Cache for performance
    descendant_cache: HashMap<String, Vec<String>>,
    branch_commit_cache: HashMap<String, String>,
}

impl App {
    fn new<P: AsRef<Path>>(repo_path: P) -> Result<Self> {
        let repo = Repository::open(repo_path)?;
        let mut app = App {
            repository: repo,
            branches: Vec::new(),
            commits: HashMap::new(),
            graph_lines: Vec::new(),
            selected_branch: 0,
            selected_commit: 0,
            branch_list_state: ListState::default(),
            commit_list_state: ListState::default(),
            show_logs: false,
            current_branch_filter: None,
            loading: false,
            error_message: None,
            scroll_offset: 0,
            current_diff: None,
            show_diff: false,
            diff_scroll_offset: 0,
            descendant_cache: HashMap::new(),
            branch_commit_cache: HashMap::new(),
        };
        
        app.load_branches()?;
        // Don't precompute all relationships - do it lazily
        app.load_graph()?;
        app.branch_list_state.select(Some(0));
        app.commit_list_state.select(Some(0));
        
        Ok(app)
    }
    
    fn load_branches(&mut self) -> Result<()> {
        self.branches.clear();
        self.branch_commit_cache.clear();
        
        // Load local branches
        let branches = self.repository.branches(Some(BranchType::Local))?;
        for branch_result in branches {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(target) = reference.target() {
                    let is_head = branch.is_head();
                    let commit_id = target.to_string();
                    
                    self.branches.push(GitBranch {
                        name: name.to_string(),
                        commit_id: commit_id.clone(),
                        is_head,
                        is_remote: false,
                    });
                    
                    // Cache commit ID for quick lookup
                    self.branch_commit_cache.insert(name.to_string(), commit_id);
                }
            }
        }
        
        // Load remote branches
        let remote_branches = self.repository.branches(Some(BranchType::Remote))?;
        for branch_result in remote_branches {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(target) = reference.target() {
                    let commit_id = target.to_string();
                    
                    self.branches.push(GitBranch {
                        name: name.to_string(),
                        commit_id: commit_id.clone(),
                        is_head: false,
                        is_remote: true,
                    });
                    
                    // Cache commit ID for quick lookup
                    self.branch_commit_cache.insert(name.to_string(), commit_id);
                }
            }
        }
        
        Ok(())
    }
    
    fn is_ancestor_fast(&self, ancestor_commit: &str, descendant_commit: &str) -> Result<bool> {
        // Use git merge-base to check if ancestor_commit is an ancestor of descendant_commit
        let mut cmd = std::process::Command::new("git");
        cmd.arg("merge-base")
           .arg("--is-ancestor")
           .arg(ancestor_commit)
           .arg(descendant_commit)
           .current_dir(self.repository.path().parent().unwrap_or(self.repository.path()))
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        
        match cmd.status() {
            Ok(status) => Ok(status.success()),
            Err(_) => Ok(false),
        }
    }
    
    fn load_graph(&mut self) -> Result<()> {
        self.commits.clear();
        self.graph_lines.clear();
        
        // Get git log output with graph using the exact same format as gn function
        let mut cmd = std::process::Command::new("git");
        cmd.arg("log")
           .arg("--graph")
           .arg("--abbrev-commit")
           .arg("--decorate")
           .arg("--date=relative")
           .arg("--format=format:%C(bold cyan)%h%C(reset) - %C(bold green)(%ar)%C(reset) %C(yellow)%s%C(reset) %C(red)- %an%C(reset)%C(bold yellow)%d%C(reset)")
           .arg("-100"); // Limit to 100 commits for better visibility while maintaining performance
        
        // If we have a branch filter, show only related branches with proper graph structure
        if let Some(ref branch_name) = self.current_branch_filter {
            // Get descendants from cache or compute on-demand
            let descendant_branches = if let Some(cached) = self.descendant_cache.get(branch_name) {
                cached.clone()
            } else {
                let descendants = self.compute_descendants_fast(branch_name)?;
                self.descendant_cache.insert(branch_name.clone(), descendants.clone());
                descendants
            };
            
            // For master branch or branches with no descendants, don't exclude gerrit refs
            // as it might exclude all commits
            if branch_name != "master" && !descendant_branches.is_empty() {
                // Get all gerrit refs to exclude (like the gn function does)
                let gerrit_output = std::process::Command::new("git")
                    .arg("for-each-ref")
                    .arg("--format=^%(refname:short)")
                    .arg("refs/remotes/gerrit/")
                    .current_dir(self.repository.path().parent().unwrap_or(self.repository.path()))
                    .output();
                
                if let Ok(gerrit_out) = gerrit_output {
                    let gerrit_refs = String::from_utf8_lossy(&gerrit_out.stdout);
                    for gerrit_ref in gerrit_refs.lines() {
                        if !gerrit_ref.contains("sunmi") {
                            cmd.arg(gerrit_ref);
                        }
                    }
                }
            }
            
            // Add the base branch
            cmd.arg(branch_name);
            
            // Add all descendant branches
            for descendant in &descendant_branches {
                cmd.arg(descendant);
            }
        } else {
            cmd.arg("--all");
        }
        
        cmd.current_dir(self.repository.path().parent().unwrap_or(self.repository.path()));
        
        let output = match cmd.output() {
            Ok(output) => output,
            Err(e) => {
                eprintln!("Failed to execute git command: {}", e);
                return Ok(());
            }
        };
        
        if !output.status.success() {
            eprintln!("Git command failed: {}", String::from_utf8_lossy(&output.stderr));
            return Ok(());
        }
        
        let git_output = String::from_utf8_lossy(&output.stdout);
        
        // Parse the git log output
        for line in git_output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            
            if let Some(commit_info) = self.parse_gn_format_line(line) {
                // Extract commit ID from the line for commit lookup
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(commit_short) = parts.iter().find(|p| p.len() >= 7 && p.chars().all(|c| c.is_ascii_hexdigit())) {
                    if let Ok(oid) = self.find_commit_by_short_id(commit_short) {
                        if let Ok(commit) = self.repository.find_commit(oid) {
                            let refs = self.extract_refs_from_line(line);
                            
                            let git_commit = GitCommit {
                                id: commit.id().to_string(),
                                short_id: commit_short.to_string(),
                                message: commit.message().unwrap_or("").to_string(), // Full message
                                author: commit.author().name().unwrap_or("Unknown").to_string(),
                                timestamp: DateTime::from_timestamp(commit.time().seconds(), 0).unwrap_or_else(|| Utc::now()),
                                parents: commit.parents().map(|p| p.id().to_string()).collect(),
                                refs,
                            };
                            
                            self.commits.insert(git_commit.id.clone(), git_commit);
                        }
                    }
                }
                self.graph_lines.push(commit_info);
            }
        }
        
        Ok(())
    }
    
    fn parse_gn_format_line(&self, line: &str) -> Option<GraphLine> {
        // Parse the gn format: graph + commit_hash - (time) message - author (refs)
        let mut graph_part = String::new();
        let mut commit_part = String::new();
        let mut commit_id = String::new();
        let mut found_commit = false;
        
        for (i, ch) in line.chars().enumerate() {
            if !found_commit && (ch == '*' || ch.is_ascii_hexdigit()) {
                // Check if this looks like a commit hash (7+ hex chars)
                let remaining = &line[i..];
                if let Some(space_pos) = remaining.find(' ') {
                    let potential_hash = &remaining[..space_pos];
                    if potential_hash.len() >= 7 && potential_hash.chars().all(|c| c.is_ascii_hexdigit() || c == '*') {
                        found_commit = true;
                        commit_part = line[i..].to_string();
                        // Extract just the commit hash
                        if ch.is_ascii_hexdigit() {
                            commit_id = potential_hash.to_string();
                        }
                        break;
                    }
                }
            }
            if !found_commit {
                graph_part.push(ch);
            }
        }
        
        // If no commit found, this might be a graph-only line
        if !found_commit {
            graph_part = line.to_string();
        }
        
        Some(GraphLine {
            graph_text: graph_part,
            commit_text: commit_part,
            commit_id,
            refs_text: String::new(),
        })
    }
    
    fn compute_descendants_fast(&self, base_branch: &str) -> Result<Vec<String>> {
        let mut descendants = Vec::new();
        
        // Get the commit ID of the base branch
        let base_commit_id = match self.branch_commit_cache.get(base_branch) {
            Some(id) => id,
            None => return Ok(descendants),
        };
        
        // Find branches that have the base branch as an ancestor
        // (i.e., branches that were created FROM the base branch)
        for branch in &self.branches {
            if branch.name == base_branch || branch.is_remote {
                continue; // Skip the base branch itself and remote branches
            }
            
            if let Some(branch_commit_id) = self.branch_commit_cache.get(&branch.name) {
                // Check if base_branch is an ancestor of this branch
                // This means this branch was created FROM the base branch
                if self.is_ancestor_fast(base_commit_id, branch_commit_id)? {
                    descendants.push(branch.name.clone());
                }
            }
        }
        
        Ok(descendants)
    }
    
    fn parse_git_log_line(&self, line: &str) -> Option<GraphLine> {
        // Find where the commit hash starts
        let mut graph_part = String::new();
        let mut commit_part = String::new();
        let mut found_commit = false;
        
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        
        // Extract graph part (everything before the commit hash)
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_ascii_hexdigit() && i + 6 < chars.len() {
                // Check if this looks like a commit hash (7+ hex chars)
                let mut is_commit_hash = true;
                let mut hash_len = 0;
                for j in i..std::cmp::min(i + 10, chars.len()) {
                    if chars[j].is_ascii_hexdigit() {
                        hash_len += 1;
                    } else if chars[j] == ' ' && hash_len >= 7 {
                        break;
                    } else {
                        is_commit_hash = false;
                        break;
                    }
                }
                
                if is_commit_hash && hash_len >= 7 {
                    // Found commit hash, everything from here is commit part
                    commit_part = chars[i..].iter().collect::<String>();
                    found_commit = true;
                    break;
                }
            }
            graph_part.push(ch);
            i += 1;
        }
        
        if !found_commit {
            return None;
        }
        
        // Extract refs if any
        let refs_text = if commit_part.contains('(') && commit_part.contains(')') {
            let start = commit_part.find('(').unwrap();
            let end = commit_part.rfind(')').unwrap();
            commit_part[start..=end].to_string()
        } else {
            String::new()
        };
        
        // Extract commit ID for lookup
        let commit_id = commit_part.split_whitespace().next().unwrap_or("").to_string();
        
        Some(GraphLine {
            commit_id,
            graph_text: graph_part,
            commit_text: commit_part,
            refs_text,
        })
    }
    
    fn colorize_graph_text(&self, graph_text: &str) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut current_span = String::new();
        let mut current_color = Color::White;
        
        for ch in graph_text.chars() {
            let new_color = match ch {
                '*' => Color::Red,        // Commit nodes
                '|' => Color::Green,      // Vertical lines  
                '/' => Color::Blue,       // Merge lines going up-right
                '\\' => Color::Cyan,      // Merge lines going down-right
                '_' => Color::Yellow,     // Horizontal lines
                '-' => Color::Yellow,     // Horizontal merge lines
                '+' => Color::Magenta,    // Complex merge points
                ' ' => Color::White,      // Spaces
                _ => Color::White,        // Other characters
            };
            
            if new_color != current_color && !current_span.is_empty() {
                spans.push(Span::styled(current_span.clone(), Style::default().fg(current_color)));
                current_span.clear();
            }
            
            current_span.push(ch);
            current_color = new_color;
        }
        
        if !current_span.is_empty() {
            spans.push(Span::styled(current_span, Style::default().fg(current_color)));
        }
        
        spans
    }
    
    fn colorize_commit_text(&self, commit_text: &str) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        
        // Split the commit text into parts
        let parts: Vec<&str> = commit_text.split_whitespace().collect();
        if parts.is_empty() {
            return spans;
        }
        
        let mut in_refs = false;
        let mut ref_content = String::new();
        
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" ".to_string())); // Add space between parts
            }
            
            // Check if this is a commit hash (7+ hex characters)
            if i == 0 && part.len() >= 7 && part.chars().all(|c| c.is_ascii_hexdigit()) {
                spans.push(Span::styled(part.to_string(), Style::default().fg(Color::Yellow)));
            }
            // Check for references (HEAD, origin, branch names)
            else if part.starts_with('(') {
                in_refs = true;
                ref_content = part.to_string();
                if part.ends_with(')') {
                    in_refs = false;
                    spans.push(self.colorize_refs(&ref_content));
                    ref_content.clear();
                }
            }
            else if in_refs {
                ref_content.push(' ');
                ref_content.push_str(part);
                if part.ends_with(')') {
                    in_refs = false;
                    spans.push(self.colorize_refs(&ref_content));
                    ref_content.clear();
                }
            }
            // Regular commit message
            else {
                spans.push(Span::styled(part.to_string(), Style::default().fg(Color::White)));
            }
        }
        
        spans
    }
    
    fn colorize_refs(&self, refs_text: &str) -> Span<'static> {
        // Remove parentheses for processing
        let inner = refs_text.trim_start_matches('(').trim_end_matches(')');
        
        // Determine color based on ref type
        let color = if inner.contains("HEAD") {
            Color::Cyan
        } else if inner.contains("origin/") || inner.contains("remote/") {
            Color::Red
        } else if inner.contains("tag:") {
            Color::Yellow
        } else {
            Color::Green // Local branches
        };
        
        Span::styled(refs_text.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD))
    }
    
    fn extract_refs_from_line(&self, line: &str) -> Vec<String> {
        let mut refs = Vec::new();
        if let Some(start) = line.find('(') {
            if let Some(end) = line.rfind(')') {
                let refs_str = &line[start+1..end];
                for part in refs_str.split(',') {
                    let part = part.trim();
                    if part.starts_with("origin/") || !part.contains('/') {
                        refs.push(part.to_string());
                    }
                }
            }
        }
        refs
    }
}

fn colorize_diff_line(line: &str) -> Line<'static> {
    if line.starts_with("+++") || line.starts_with("---") {
        // File headers
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)))
    } else if line.starts_with("@@") {
        // Hunk headers
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
    } else if line.starts_with("+") {
        // Added lines
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Green)))
    } else if line.starts_with("-") {
        // Removed lines
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Red)))
    } else if line.starts_with("commit ") {
        // Commit hash
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
    } else if line.starts_with("Author: ") || line.starts_with("Date: ") {
        // Author and date info
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Blue)))
    } else if line.starts_with("diff --git") {
        // Diff headers
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
    } else if line.starts_with("index ") {
        // Index line
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::Gray)))
    } else {
        // Normal text
        Line::from(Span::styled(line.to_string(), Style::default().fg(Color::White)))
    }
}

impl App {
    fn find_commit_by_short_id(&self, short_id: &str) -> Result<Oid> {
        // Try to expand the short ID using git2's built-in functionality
        match self.repository.revparse_single(short_id) {
            Ok(obj) => {
                if let Some(commit) = obj.as_commit() {
                    return Ok(commit.id());
                }
                if let Some(tag) = obj.as_tag() {
                    if let Some(commit) = tag.target()?.as_commit() {
                        return Ok(commit.id());
                    }
                }
                return Ok(obj.id());
            }
            Err(_) => {
                // Fallback: try to find manually
                let mut revwalk = self.repository.revwalk()?;
                revwalk.push_head().ok(); // Don't fail if HEAD doesn't exist
                revwalk.set_sorting(git2::Sort::TIME)?;
                
                for commit_id in revwalk.take(1000) { // Limit search to recent 1000 commits
                    if let Ok(commit_id) = commit_id {
                        let commit_str = commit_id.to_string();
                        if commit_str.starts_with(short_id) {
                            return Ok(commit_id);
                        }
                    }
                }
            }
        }
        
        Err(anyhow::anyhow!("Commit not found: {}", short_id))
    }
    
    fn set_branch_filter(&mut self, branch_name: Option<String>) {
        self.current_branch_filter = branch_name;
        self.loading = true;
        self.error_message = None;
        match self.load_graph() {
            Ok(_) => {
                self.loading = false;
            }
            Err(e) => {
                self.loading = false;
                self.error_message = Some(format!("Failed to load graph: {}", e));
            }
        }
        self.commit_list_state.select(Some(0));
        self.selected_commit = 0;
    }
    
    fn get_included_branches(&self) -> Vec<String> {
        if let Some(ref base_branch) = self.current_branch_filter {
            let mut included = vec![base_branch.clone()];
            if let Some(descendants) = self.descendant_cache.get(base_branch) {
                included.extend(descendants.clone());
            }
            included
        } else {
            Vec::new()
        }
    }
    
    fn refresh_data(&mut self) -> Result<()> {
        self.loading = true;
        self.error_message = None;
        
        match self.load_branches() {
            Ok(_) => {
                match self.load_graph() {
                    Ok(_) => {
                        self.loading = false;
                    }
                    Err(e) => {
                        self.loading = false;
                        self.error_message = Some(format!("Failed to load graph: {}", e));
                    }
                }
            }
            Err(e) => {
                self.loading = false;
                self.error_message = Some(format!("Failed to load branches: {}", e));
            }
        }
        
        Ok(())
    }
    
    fn next_branch(&mut self) {
        if !self.branches.is_empty() {
            self.selected_branch = (self.selected_branch + 1) % self.branches.len();
            self.branch_list_state.select(Some(self.selected_branch));
        }
    }
    
    fn previous_branch(&mut self) {
        if !self.branches.is_empty() {
            self.selected_branch = if self.selected_branch == 0 {
                self.branches.len() - 1
            } else {
                self.selected_branch - 1
            };
            self.branch_list_state.select(Some(self.selected_branch));
        }
    }
    
    fn next_commit(&mut self) {
        if !self.graph_lines.is_empty() {
            self.selected_commit = (self.selected_commit + 1) % self.graph_lines.len();
            self.commit_list_state.select(Some(self.selected_commit));
        }
    }
    
    fn previous_commit(&mut self) {
        if !self.graph_lines.is_empty() {
            self.selected_commit = if self.selected_commit == 0 {
                self.graph_lines.len() - 1
            } else {
                self.selected_commit - 1
            };
            self.commit_list_state.select(Some(self.selected_commit));
        }
    }
    
    fn get_selected_commit(&self) -> Option<&GitCommit> {
        if let Some(line) = self.graph_lines.get(self.selected_commit) {
            // First try to find by exact commit_id match
            if !line.commit_id.is_empty() {
                if let Some(commit) = self.commits.values().find(|c| 
                    c.short_id == line.commit_id || 
                    c.id.starts_with(&line.commit_id) || 
                    c.id == line.commit_id
                ) {
                    return Some(commit);
                }
            }
            
            // Fallback: try to extract commit hash from commit_text
            let parts: Vec<&str> = line.commit_text.split_whitespace().collect();
            if let Some(potential_hash) = parts.first() {
                if potential_hash.len() >= 7 && potential_hash.chars().all(|c| c.is_ascii_hexdigit()) {
                    if let Some(commit) = self.commits.values().find(|c| 
                        c.short_id == *potential_hash || 
                        c.id.starts_with(potential_hash)
                    ) {
                        return Some(commit);
                    }
                }
            }
        }
        None
    }
    
    fn select_current_branch(&mut self) {
        if let Some(branch) = self.branches.get(self.selected_branch) {
            let branch_name = if branch.is_remote {
                branch.name.clone()
            } else {
                branch.name.clone()
            };
            self.set_branch_filter(Some(branch_name));
        }
    }
    
    fn clear_branch_filter(&mut self) {
        self.set_branch_filter(None);
    }
    
    fn load_commit_diff(&mut self) {
        if self.graph_lines.is_empty() || self.selected_commit >= self.graph_lines.len() {
            return;
        }
        
        let selected_line = &self.graph_lines[self.selected_commit];
        let commit_id = &selected_line.commit_id;
        
        if commit_id.is_empty() {
            return;
        }
        
        // Run git show command to get diff (no color to avoid ANSI codes)
        let output = std::process::Command::new("git")
            .args(&["show", "--no-color", "--format=fuller", "--stat", "-p", commit_id])
            .current_dir(self.repository.workdir().unwrap_or_else(|| self.repository.path()))
            .output();
            
        match output {
            Ok(output) => {
                if output.status.success() {
                    self.current_diff = Some(String::from_utf8_lossy(&output.stdout).to_string());
                    self.show_diff = true;
                    self.diff_scroll_offset = 0;
                } else {
                    self.current_diff = Some(format!("Error getting diff: {}", 
                        String::from_utf8_lossy(&output.stderr)));
                    self.show_diff = true;
                    self.diff_scroll_offset = 0;
                }
            }
            Err(e) => {
                self.current_diff = Some(format!("Failed to run git show: {}", e));
                self.show_diff = true;
                self.diff_scroll_offset = 0;
            }
        }
    }
    
    fn close_diff(&mut self) {
        self.show_diff = false;
        self.current_diff = None;
        self.diff_scroll_offset = 0;
    }

    fn get_max_diff_scroll(&self, visible_height: u16) -> u16 {
        if let Some(ref diff_content) = self.current_diff {
            let total_lines = diff_content.lines().count();
            let content_height = (visible_height.saturating_sub(2)) as usize; // Account for borders
            total_lines.saturating_sub(content_height) as u16
        } else {
            0
        }
    }

    fn clamp_diff_scroll(&mut self, visible_height: u16) {
        let max_scroll = self.get_max_diff_scroll(visible_height);
        self.diff_scroll_offset = self.diff_scroll_offset.min(max_scroll);
    }
}

fn draw_ui(f: &mut Frame, app: &mut App) {
    // Main layout with help at bottom
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)]) // Help takes 3 lines at bottom
        .split(f.area());
    
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(33), Constraint::Percentage(67)]) // Graph takes 2/3
        .split(main_chunks[0]);
    
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[0]);
    
    // Draw branches (top-left)
    draw_branches(f, app, left_chunks[0]);
    
    // Draw commit details (bottom-left)
    draw_commit_details(f, app, left_chunks[1]);
    
    // Draw commits graph (right side)
    draw_commits(f, app, chunks[1]);
    
    // Draw help at bottom
    draw_help(f, app, main_chunks[1]);
    
    // Draw diff overlay if showing diff
    if app.show_diff {
        draw_diff_overlay(f, app);
    }
}

fn draw_branches(f: &mut Frame, app: &mut App, area: Rect) {
    let included_branches = app.get_included_branches();
    
    let items: Vec<ListItem> = app.branches
        .iter()
        .map(|branch| {
            let is_current_filter = app.current_branch_filter.as_ref() == Some(&branch.name);
            let is_included = included_branches.contains(&branch.name);
            
            let style = if is_current_filter {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if is_included {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if branch.is_head {
                Style::default().fg(Color::Yellow)
            } else if branch.is_remote {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            
            let marker = if is_current_filter { 
                "● " 
            } else if is_included { 
                "◉ " 
            } else { 
                "○ " 
            };
            let head_marker = if branch.is_head { " (HEAD)" } else { "" };
            let remote_marker = if branch.is_remote { " [remote]" } else { "" };
            
            ListItem::new(format!("{}{}{}{}", marker, branch.name, head_marker, remote_marker))
                .style(style)
        })
        .collect();
    
    let title = "Branches";
    
    // Highlight the border when this panel is focused
    let border_style = if !app.show_logs {
        Style::default().fg(Color::Yellow)  // Active panel: yellow border
    } else {
        Style::default().fg(Color::DarkGray)  // Inactive panel: dark gray border
    };
    
    let list = List::new(items)
        .block(Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style))
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    
    f.render_stateful_widget(list, area, &mut app.branch_list_state);
}

fn draw_commits(f: &mut Frame, app: &mut App, area: Rect) {
    if app.loading {
        let paragraph = Paragraph::new("Loading git graph...")
            .block(Block::default()
                .title("Git Graph")
                .borders(Borders::ALL));
        f.render_widget(paragraph, area);
        return;
    }
    
    if let Some(ref error) = app.error_message {
        let paragraph = Paragraph::new(format!("Error: {}", error))
            .block(Block::default()
                .title("Git Graph - Error")
                .borders(Borders::ALL))
            .style(Style::default().fg(Color::Red));
        f.render_widget(paragraph, area);
        return;
    }
    
    // Pre-compute colored lines to avoid borrowing issues
    let colored_lines: Vec<Line> = app.graph_lines
        .iter()
        .map(|line| {
            // Create colored spans for graph and commit text
            let mut spans = Vec::new();
            
            // Add colored graph part
            spans.extend(app.colorize_graph_text(&line.graph_text));
            
            // Add colored commit part
            spans.extend(app.colorize_commit_text(&line.commit_text));
            
            // Create a Line from spans
            Line::from(spans)
        })
        .collect();
    
    let items: Vec<ListItem> = colored_lines
        .into_iter()
        .map(|line| ListItem::new(line))
        .collect();
    
    let title = if let Some(ref branch) = app.current_branch_filter {
        let included_branches = app.get_included_branches();
        if included_branches.len() > 1 {
            format!("Git Graph - {} + {} descendants", 
                    branch, included_branches.len() - 1)
        } else {
            format!("Git Graph - {}", branch)
        }
    } else {
        "Git Graph - All branches".to_string()
    };
    
    // Highlight the border when this panel is focused
    let border_style = if app.show_logs {
        Style::default().fg(Color::Yellow)  // Active panel: yellow border
    } else {
        Style::default().fg(Color::DarkGray)  // Inactive panel: dark gray border
    };
    
    let list = List::new(items)
        .block(Block::default()
            .title(title.as_str())
            .borders(Borders::ALL)
            .border_style(border_style))
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");
    
    f.render_stateful_widget(list, area, &mut app.commit_list_state);
}

fn draw_commit_details(f: &mut Frame, app: &App, area: Rect) {
    let content = if let Some(commit) = app.get_selected_commit() {
        let mut details = format!(
            "Commit: {}\nShort: {}\nAuthor: {}\nDate: {}\n",
            commit.id,
            commit.short_id,
            commit.author,
            commit.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        );
        
        if !commit.parents.is_empty() {
            details.push_str(&format!("\nParents:\n"));
            for parent in &commit.parents {
                details.push_str(&format!("  {}\n", &parent[..8]));
            }
        }
        
        // Add full commit message with proper formatting
        details.push_str(&format!("\nMessage:\n{}", commit.message));
        
        details
    } else {
        // Debug information to see what's happening
        let selected_line = app.graph_lines.get(app.selected_commit);
        let debug_info = if let Some(line) = selected_line {
            format!("❌ No commit found!\n\nSelected Line:\n• Index: {}\n• Commit ID: '{}'\n• Graph: '{}'\n• Commit Text: '{}'\n\nCommits in HashMap: {}", 
                    app.selected_commit, line.commit_id, line.graph_text, line.commit_text, app.commits.len())
        } else {
            format!("❌ No line at index {} (total: {})", app.selected_commit, app.graph_lines.len())
        };
        
        format!("🐛 DEBUG MODE\n\nShow Logs: {}\n{}", app.show_logs, debug_info)
    };
    
    // Commit details panel has a neutral style (always dark gray since it's not directly navigable)
    let paragraph = Paragraph::new(content)
        .block(Block::default()
            .title("Commit Details")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)))
        .wrap(Wrap { trim: true })
        .scroll((app.scroll_offset, 0)); // Add scrolling capability
    
    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let help_text = if app.show_diff {
        "Esc/q: close diff  ↑/↓/j/k: scroll  PgUp/PgDn: scroll fast"
    } else if app.show_logs {
        "Tab/h/l: switch panel  c: clear filter  r: refresh  q: quit  |  ↑/↓/j/k: navigate  PgUp/PgDn: scroll  Enter: diff"
    } else {
        "Tab/h/l: switch panel  c: clear filter  r: refresh  q: quit  |  ↑/↓/j/k: navigate  Enter: select branch"
    };
    
    let help = Paragraph::new(help_text)
        .block(Block::default()
            .title("Help")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)))
        .style(Style::default().fg(Color::Gray));
    
    f.render_widget(help, area);
}

fn draw_diff_overlay(f: &mut Frame, app: &mut App) {
    // Create a centered overlay that takes 90% of the screen
    let area = f.area();
    let popup_area = Rect {
        x: area.width / 20, // 5% margin
        y: area.height / 20, // 5% margin  
        width: area.width * 9 / 10, // 90% width
        height: area.height * 9 / 10, // 90% height
    };
    
    // Clamp scroll offset to prevent over-scrolling
    app.clamp_diff_scroll(popup_area.height);
    
    // Clear only the popup area  
    f.render_widget(Clear, popup_area);
    
    if let Some(ref diff_content) = app.current_diff {
        let lines: Vec<&str> = diff_content.lines().collect();
        let visible_lines: Vec<&str> = lines
            .iter()
            .skip(app.diff_scroll_offset as usize)
            .take((popup_area.height.saturating_sub(2)) as usize) // Account for borders
            .copied()
            .collect();
        
        // Create colorized spans for diff content
        let mut styled_lines = Vec::new();
        for line in visible_lines {
            styled_lines.push(colorize_diff_line(line));
        }
        
        let paragraph = Paragraph::new(styled_lines)
            .block(Block::default()
                .title(format!(" Diff (line {}/{}) ", 
                    app.diff_scroll_offset + 1, 
                    lines.len().max(1)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)))
            .wrap(Wrap { trim: false });
        
        f.render_widget(paragraph, popup_area);
    } else {
        let paragraph = Paragraph::new("Loading diff...")
            .block(Block::default()
                .title(" Diff ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)))
            .style(Style::default().fg(Color::Gray));
        
        f.render_widget(paragraph, popup_area);
    }
}

fn handle_events(app: &mut App) -> Result<bool> {
    if event::poll(std::time::Duration::from_millis(50))? { // Reduced timeout for faster response
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                // Handle diff view separately
                if app.show_diff {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.close_diff();
                            return Ok(false);
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if app.diff_scroll_offset > 0 {
                                app.diff_scroll_offset -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            // Calculate current popup height (90% of terminal height)
                            let terminal_height = crossterm::terminal::size().unwrap_or((80, 24)).1;
                            let popup_height = terminal_height * 9 / 10;
                            let max_scroll = app.get_max_diff_scroll(popup_height);
                            if app.diff_scroll_offset < max_scroll {
                                app.diff_scroll_offset += 1;
                            }
                        }
                        KeyCode::PageUp => {
                            if app.diff_scroll_offset >= 10 {
                                app.diff_scroll_offset -= 10;
                            } else {
                                app.diff_scroll_offset = 0;
                            }
                        }
                        KeyCode::PageDown => {
                            // Calculate current popup height (90% of terminal height)
                            let terminal_height = crossterm::terminal::size().unwrap_or((80, 24)).1;
                            let popup_height = terminal_height * 9 / 10;
                            let max_scroll = app.get_max_diff_scroll(popup_height);
                            app.diff_scroll_offset = (app.diff_scroll_offset + 10).min(max_scroll);
                        }
                        _ => {}
                    }
                    return Ok(false);
                }
                
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
                    KeyCode::Up | KeyCode::Char('k') => {
                        if app.show_logs {
                            app.previous_commit();
                        } else {
                            app.previous_branch();
                        }
                        app.scroll_offset = 0; // Reset scroll when changing commits
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.show_logs {
                            app.next_commit();
                        } else {
                            app.next_branch();
                        }
                        app.scroll_offset = 0; // Reset scroll when changing commits
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        // Switch to branch panel if currently in logs
                        if app.show_logs {
                            app.show_logs = false;
                            if !app.branches.is_empty() {
                                app.branch_list_state.select(Some(app.selected_branch));
                            }
                        }
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        // Switch to git graph panel if currently in branches
                        if !app.show_logs {
                            app.show_logs = true;
                            if !app.graph_lines.is_empty() {
                                app.commit_list_state.select(Some(app.selected_commit));
                            }
                        }
                    }
                    KeyCode::PageUp => {
                        if app.scroll_offset > 5 {
                            app.scroll_offset -= 5;
                        } else {
                            app.scroll_offset = 0;
                        }
                    }
                    KeyCode::PageDown => {
                        app.scroll_offset += 5;
                    }
                    KeyCode::Tab => {
                        app.show_logs = !app.show_logs;
                        // Ensure the commit list state is properly initialized when switching to logs view
                        if app.show_logs && !app.graph_lines.is_empty() {
                            app.commit_list_state.select(Some(app.selected_commit));
                        }
                        // Ensure the branch list state is properly initialized when switching to branch view
                        if !app.show_logs && !app.branches.is_empty() {
                            app.branch_list_state.select(Some(app.selected_branch));
                        }
                    }
                    KeyCode::Enter => {
                        if !app.show_logs {
                            // Branches 패널에서 Enter: 브랜치 선택
                            app.select_current_branch();
                        } else {
                            // Git Graph 패널에서 Enter: diff 보기
                            app.load_commit_diff();
                        }
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') => {
                        app.clear_branch_filter();
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        let _ = app.refresh_data();
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(false)
}

fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    
    // Find git repository
    let repo_path = std::env::current_dir()?;
    let repo_path = Repository::discover(&repo_path)?;
    
    // Create app
    let mut app = App::new(repo_path.path())?;
    
    // Main loop
    let result = run_app(&mut terminal, &mut app);
    
    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    
    if let Err(err) = result {
        println!("Error: {:?}", err);
    }
    
    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw_ui(f, app))?;
        
        if handle_events(app)? {
            break;
        }
    }
    Ok(())
}
