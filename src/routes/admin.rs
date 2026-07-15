use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminPermission {
    pub(crate) key: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) fn admin_permissions() -> Vec<AdminPermission> {
    vec![
        AdminPermission {
            key: "songs:add",
            description: "add songs to the radio catalog",
        },
        AdminPermission {
            key: "radio:control",
            description: "control radio playback and queue state",
        },
    ]
}
