/** Time-formatting helpers shared across views. */

/** Options for {@link relativeTimeAgo}. */
export interface RelativeTimeOptions {
	/** Label returned for a null/empty value (default `''`). */
	empty?: string;
	/** When the value can't be parsed, return it stringified instead of `empty`. */
	invalidAsString?: boolean;
}

/** Unit thresholds in seconds, largest first. Hoisted so it isn't rebuilt per call. */
const RELATIVE_UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
	['year', 31536000],
	['month', 2592000],
	['week', 604800],
	['day', 86400],
	['hour', 3600],
	['minute', 60]
];

/**
 * Lazily-built, reused `Intl.RelativeTimeFormat`. Constructing one is ~orders of
 * magnitude costlier than a `format()` call, and {@link relativeTimeAgo} runs
 * once per row per render across large lists — so we build it once (browser
 * default locale, matching the previous `undefined` argument) and reuse it.
 */
let relativeFormatter: Intl.RelativeTimeFormat | undefined;
function getRelativeFormatter(): Intl.RelativeTimeFormat {
	if (!relativeFormatter) {
		relativeFormatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
	}
	return relativeFormatter;
}

/**
 * Locale-aware relative "time ago" via `Intl.RelativeTimeFormat`.
 *
 * Accepts an epoch (seconds or milliseconds) or an ISO-8601 string, so the
 * grid-card metadata lines (epoch) and the profile/app-password tables (ISO)
 * share one implementation.
 */
export function relativeTimeAgo(
	value: number | string | null | undefined,
	opts: RelativeTimeOptions = {}
): string {
	const empty = opts.empty ?? '';
	if (value === null || value === undefined || value === '') return empty;
	const date =
		typeof value === 'number' ? new Date(value < 1e12 ? value * 1000 : value) : new Date(value);
	if (Number.isNaN(date.getTime())) return opts.invalidAsString ? String(value) : empty;

	const diffSec = Math.round((date.getTime() - Date.now()) / 1000);
	const abs = Math.abs(diffSec);
	const rtf = getRelativeFormatter();
	for (const [unit, secs] of RELATIVE_UNITS) {
		if (abs >= secs) return rtf.format(Math.round(diffSec / secs), unit);
	}
	return rtf.format(diffSec, 'second');
}
