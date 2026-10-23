//! Comp node groups / macros (fork C4.3, ADR 0009 §2): `comp.group`, `comp.ungroup`,
//! `comp.group.expose`, `comp.group.unexpose`. Each is one undo step through [`edit_comp`]; the
//! graph surgery lives in [`filmcraft_project::comp::group`]. A group's exposed parameters are
//! ordinary parameters of the group node: `comp.setParam` / `comp.toggleAnimation` set them by
//! their id. With `group` (C4.8) each works one level inside the named group, so a nested group
//! is grouped, ungrouped or exposed on in place.

use filmcraft_project::comp::{MAX_LABEL, group};
use filmcraft_project::dag::{self, NodeId};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, str_p, u64_p};
use crate::comp::edit_comp;
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("comp.group", "Group Nodes", r#"{"comp":id?,"group":[id]?,"nodes":[id],"label":str?}"#, group_cmd),
        spec("comp.ungroup", "Ungroup", r#"{"comp":id?,"group":[id]?,"node":id}"#, ungroup_cmd),
        spec("comp.group.expose", "Expose Group Parameter", r#"{"comp":id?,"group":[id]?,"node":id,"inner":id,"param":str,"label":str?}"#, expose_cmd),
        spec("comp.group.unexpose", "Remove Group Parameter", r#"{"comp":id?,"group":[id]?,"node":id,"id":str}"#, unexpose_cmd),
    ]
}

fn node_p(p: &Value, key: &str, cmd: &str) -> Result<NodeId> {
    u64_p(p, key).ok_or_else(|| bad(cmd, format!("need `{key}` (a node id)")))
}

fn label_p(p: &Value) -> String {
    str_p(p, "label").map(|l| l.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect()).unwrap_or_default()
}

fn group_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.group";
    let ids: Vec<NodeId> = match p.get("nodes") {
        Some(Value::Array(a)) if a.len() <= dag::MAX_NODES => {
            a.iter().map(|v| v.as_u64().ok_or_else(|| bad(CMD, "`nodes` must be node ids"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(CMD, format!("`nodes` must be a list of at most {} node ids", dag::MAX_NODES))),
        None => return Err(bad(CMD, "need `nodes` (the node ids to group)")),
    };
    let label = label_p(p);
    let node = edit_comp(s, p, CMD, "Group Nodes", None, |c| group::group_nodes(c, &ids, &label).map_err(|e| bad(CMD, e)))?;
    Ok(json!({ "node": node }))
}

fn ungroup_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.ungroup";
    let node = node_p(p, "node", CMD)?;
    let nodes = edit_comp(s, p, CMD, "Ungroup", None, |c| group::ungroup(c, node).map_err(|e| bad(CMD, e)))?;
    Ok(json!({ "nodes": nodes }))
}

fn expose_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.group.expose";
    let node = node_p(p, "node", CMD)?;
    let inner = node_p(p, "inner", CMD)?;
    let param = str_p(p, "param").ok_or_else(|| bad(CMD, "need `param` (the inner node's parameter)"))?.to_string();
    let label = label_p(p);
    let id = edit_comp(s, p, CMD, "Expose Group Parameter", None, |c| group::expose(c, node, inner, &param, &label).map_err(|e| bad(CMD, e)))?;
    Ok(json!({ "id": id }))
}

fn unexpose_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.group.unexpose";
    let node = node_p(p, "node", CMD)?;
    let id = str_p(p, "id").ok_or_else(|| bad(CMD, "need `id` (the exposed parameter's id)"))?.to_string();
    // C4.8 review: a setting the enclosing group puts on itself again stays (taking it off here
    // would silently drop the outer one)
    let path = crate::comp::group_p(p, CMD)?;
    if let Some((&outer, parent)) = path.split_last() {
        let item = crate::comp::target(s, p, CMD)?;
        let top = crate::comp::comp_of(&s.project, item).ok_or_else(|| bad(CMD, "not a comp"))?;
        let level = if parent.is_empty() { Some(top) } else { group::inner(top, parent).ok() };
        if let Some(filmcraft_project::comp::CompKind::Group { exposed, .. }) = level.and_then(|l| l.node(outer)).map(|n| &n.kind)
            && let Some(e) = exposed.iter().find(|e| e.node == node && e.param == id)
        {
            let name = match level.and_then(|l| l.node(outer)).map(|n| n.label.as_str()).filter(|l| !l.is_empty()) {
                Some(l) => format!("\u{201c}{l}\u{201d}, group {outer}"),
                None => format!("group {outer}"),
            };
            return Err(bad(
                CMD,
                format!(
                    "the group around this one ({name}) puts this setting on itself as `{}`; remove it there first (comp.group.unexpose on group {outer})",
                    e.id
                ),
            ));
        }
    }
    edit_comp(s, p, CMD, "Remove Group Parameter", None, |c| group::unexpose(c, node, &id).map_err(|e| bad(CMD, e)))?;
    Ok(Value::Null)
}
