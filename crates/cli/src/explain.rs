//! `abl explain`: what an OpenEdge error means and what to do about it, from a
//! table of common messages bundled with abl.

use abl_core::messages::{self, Message};

fn print(m: &Message) {
    println!("{}", m.text);
    println!();
    println!("{}", wrap(&m.explanation, 80));
    if !m.fixes.is_empty() {
        println!();
        println!("What to do:");
        for f in &m.fixes {
            let w = wrap(f, 76);
            let mut lines = w.lines();
            if let Some(first) = lines.next() {
                println!("  - {first}");
            }
            for l in lines {
                println!("    {l}");
            }
        }
    }
    if !m.see.is_empty() {
        let (rules, nums): (Vec<&String>, Vec<&String>) =
            m.see.iter().partition(|s| s.parse::<u32>().is_err());
        println!();
        if !rules.is_empty() {
            let list: Vec<&str> = rules.iter().map(|s| s.as_str()).collect();
            println!("Lint rule that catches it: {}", list.join(", "));
        }
        if !nums.is_empty() {
            let list: Vec<String> = nums.iter().map(|n| format!("abl explain {n}")).collect();
            println!("See also: {}", list.join(", "));
        }
    }
}

fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for word in text.split_whitespace() {
        if col > 0 && col + 1 + word.len() > width {
            out.push('\n');
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += word.len();
    }
    out
}

pub fn run(query: &[String], list: bool) -> Result<u8, String> {
    let all = messages::all();
    if list || query.is_empty() {
        println!("Messages abl can explain ({}):\n", all.len());
        for m in all {
            println!("  {:>5}  {}", m.number, m.text);
        }
        println!("\nThe texts are the typical form; wording varies between OpenEdge releases.");
        return Ok(0);
    }
    let input = query.join(" ");
    if let Some(n) = messages::number_in(&input) {
        return match messages::by_number(n) {
            Some(m) => {
                print(m);
                Ok(0)
            }
            None => {
                println!(
                    "Message {n} is not in abl's table of {} common messages.",
                    all.len()
                );
                println!(
                    "`abl explain --list` shows them; your OpenEdge documentation has the rest."
                );
                Ok(1)
            }
        };
    }
    let hits = messages::search(&input);
    match hits.as_slice() {
        [] => {
            println!("No message matches \"{input}\". `abl explain --list` shows all of them.");
            Ok(1)
        }
        [m] => {
            print(m);
            Ok(0)
        }
        many => {
            println!("{} messages match \"{input}\":\n", many.len());
            for m in many {
                println!("  {:>5}  {}", m.number, m.text);
            }
            println!("\nRun `abl explain <number>` for one of them.");
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_on_words() {
        assert_eq!(wrap("aa bb cc dd", 5), "aa bb\ncc dd");
        assert_eq!(wrap("", 5), "");
    }
}
