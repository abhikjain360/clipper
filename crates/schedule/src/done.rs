use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoneMark {
    pub item_id: Uuid,
    pub occurrence_key: String,
    pub done: bool,
}

impl DoneMark {
    pub const COLLECTION_NAME: &'static str = "schedule.done";

    pub fn row_id(&self) -> Uuid {
        Uuid::new_v5(&self.item_id, self.occurrence_key.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_and_undo_share_a_row_but_other_occurrences_do_not() {
        let mark = DoneMark {
            item_id: Uuid::new_v4(),
            occurrence_key: "date:2026-10-08".into(),
            done: true,
        };
        let mut undo = mark.clone();
        undo.done = false;
        assert_eq!(mark.row_id(), undo.row_id());
        undo.occurrence_key = "date:2026-10-09".into();
        assert_ne!(mark.row_id(), undo.row_id());
        undo.occurrence_key.clone_from(&mark.occurrence_key);
        undo.item_id = Uuid::new_v4();
        assert_ne!(mark.row_id(), undo.row_id());
    }
}
