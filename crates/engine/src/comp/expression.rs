//! Parameter expression commands (fork C4.5, ADR 0009 §1.7): `comp.setExpression` and
//! `comp.clearExpression`. Expressions work on a node's own parameters, a Color node's grade
//! settings (numbers and wheels, LO.4) and a Text node's layer settings (LO.4) of number, point,
//! colour, checkbox and menu types; they live with the parameter they drive
//! ([`filmcraft_project::comp::CompNode::param_store`]). Tracker and Planar Tracker settings take
//! none: their analysed tracks are read at several times per frame (the reference frame, the
//! Stabilise smoothing window), which a per-frame expression can't drive. Each command is one
//! undo step; a refused command changes nothing.

use filmcraft_project::expr::{self, ExprError, MAX_SOURCE};
use filmcraft_project::{Param, ParamKind};
use serde_json::{Value, json};

use filmcraft_project::grade::NodeParamKind;

use super::{Slot, comp_at, default_of, edit_comp, node_mut, node_p, params_of, slot, spec, ticks_p};
use crate::commands::{CommandSpec, bad, bool_p, str_p};
use crate::{Result, Session};

pub(super) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "comp.setExpression",
            "Set Parameter Expression",
            r#"{"comp":id?,"group":[id]?,"node":id,"param":str,"expression":str,"allowError":bool?,"time":ticks?}"#,
            set_expression,
        ),
        spec("comp.clearExpression", "Clear Parameter Expression", r#"{"comp":id?,"group":[id]?,"node":id,"param":str}"#, clear_expression),
    ]
}

/// An expression error as JSON (`message`, 1-based `column` when it has one, the full `text`).
pub(crate) fn error_json(e: &ExprError) -> Value {
    json!({"message": e.message, "column": (e.column > 0).then_some(e.column), "text": e.to_string()})
}

/// The parameter `param` of node `node` can take an expression: one of the node's own schema
/// parameters, a Color node's grade number or wheel, or a Text node's layer setting, of a type
/// expressions can produce.
fn check_param(s: &Session, p: &Value, cmd: &str) -> Result<(u64, String, Slot)> {
    let node = node_p(p, "node", cmd)?;
    let key = str_p(p, "param").ok_or_else(|| bad(cmd, "need `param`"))?.to_string();
    let c = comp_at(s, p, cmd)?.1;
    let n = c.node(node).ok_or_else(|| bad(cmd, format!("no node {node}")))?;
    let sl = slot(n, &key, cmd)?;
    match &sl {
        Slot::Own(default, kind) | Slot::Layer(default, kind) => {
            if matches!(kind, ParamKind::Text | ParamKind::Curve { .. } | ParamKind::Path) || !expr::kind_allowed(default) {
                return Err(bad(cmd, format!("`{key}` is a text, curve or path parameter: those don't take expressions")));
            }
        }
        Slot::Grade(NodeParamKind::Curve) => return Err(bad(cmd, format!("`{key}` is a curve: curves don't take expressions"))),
        Slot::Grade(_) => {}
    }
    Ok((node, key, sl))
}

fn set_expression(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.setExpression";
    let (node, key, sl) = check_param(s, p, CMD)?;
    let src = p.get("expression").and_then(Value::as_str).ok_or_else(|| bad(CMD, "need `expression` (text; empty clears it)"))?;
    if src.len() > MAX_SOURCE {
        return Err(bad(CMD, format!("the expression is longer than {MAX_SOURCE} bytes")));
    }
    let allow_error = bool_p(p, "allowError").unwrap_or(false);
    let time = ticks_p(p, "time", CMD)?.unwrap_or_default();
    let src = src.trim();
    // a parse error is returned and nothing stored (unless allowError)
    let parse_error = if src.is_empty() { None } else { expr::parse(src).err().map(|e| e.at(src)) };
    if let Some(e) = &parse_error
        && !allow_error
    {
        return Ok(json!({"stored": false, "expression": src, "error": error_json(e)}));
    }
    let stored = (!src.is_empty()).then(|| src.to_string());
    let had = comp_at(s, p, CMD)?.1.node(node).and_then(|n| n.param_store().get(&key)).is_some_and(|p| p.expression.is_some());
    if stored.is_none() && !had {
        return Ok(json!({"stored": true, "expression": Value::Null, "error": Value::Null}));
    }
    let label = if stored.is_some() { "Set Expression" } else { "Clear Expression" };
    edit_comp(s, p, CMD, label, None, |c| {
        let n = node_mut(c, node, CMD)?;
        let map = params_of(n, &sl);
        match &stored {
            Some(e) => map.entry(key.clone()).or_insert_with(|| Param::new(default_of(&sl))).expression = Some(e.clone()),
            None => {
                if let Some(prm) = map.get_mut(&key) {
                    prm.expression = None;
                }
            }
        }
        Ok(())
    })?;
    // the result now (and the evaluation error, if any)
    let c = comp_at(s, p, CMD)?.1;
    let (value, error) = c.param_at(node, &key, time);
    let error = parse_error.or(error);
    Ok(json!({
        "stored": true,
        "expression": stored,
        "value": serde_json::to_value(&value).unwrap_or(Value::Null),
        "error": error.as_ref().map(error_json),
    }))
}

fn clear_expression(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.clearExpression";
    let node = node_p(p, "node", CMD)?;
    let key = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param`"))?.to_string();
    let has =
        comp_at(s, p, CMD)?.1.node(node).ok_or_else(|| bad(CMD, format!("no node {node}")))?.param_store().get(&key).is_some_and(|p| p.expression.is_some());
    if !has {
        return Err(bad(CMD, format!("`{key}` of node {node} has no expression")));
    }
    edit_comp(s, p, CMD, "Clear Expression", None, |c| {
        if let Some(prm) = node_mut(c, node, CMD)?.param_store_mut().get_mut(&key) {
            prm.expression = None;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}
