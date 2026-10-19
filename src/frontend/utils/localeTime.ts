// WebKitGTK may report "C" for the POSIX locale. Intl rejects that language tag;
// let onboarding keep the existing 24-hour default instead of aborting its mount.
export function uses12HourClock(locale: string): boolean {
    try {
        return Intl.DateTimeFormat(locale, { hour: "numeric" }).resolvedOptions().hour12 === true
    } catch {
        return false
    }
}
