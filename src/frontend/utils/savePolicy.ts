import type { SaveActions } from "../../types/Save"

// Keep this condition aligned with the backend: this operation writes local
// settings and a backup, but does not persist the pending show/library edits.
export function isBackupOnly(customTriggers: SaveActions = {}, closeWhenFinished = false) {
    return !!customTriggers.backup && !!customTriggers.isAutoBackup && !customTriggers.autosave && !closeWhenFinished
}
