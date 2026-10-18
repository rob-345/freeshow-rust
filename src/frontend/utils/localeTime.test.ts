import { describe, expect, it } from "vitest"
import { uses12HourClock } from "./localeTime"

describe("onboarding locale time format", () => {
    it("preserves the locale hour cycle", () => {
        expect(uses12HourClock("en-US")).toBe(true)
        expect(uses12HourClock("en-GB")).toBe(false)
        expect(uses12HourClock("de-DE")).toBe(false)
    })

    it("keeps the 24-hour default for POSIX or invalid locale tags", () => {
        expect(uses12HourClock("C")).toBe(false)
        expect(uses12HourClock("")).toBe(false)
        expect(uses12HourClock("invalid_locale")).toBe(false)
    })
})
