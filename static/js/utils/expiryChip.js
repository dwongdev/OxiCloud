/**
 * buildExpiryChip — shared compact expiry editor chip.
 *
 * Chip states:
 *   • "∞ No expiry"        — dashed border, faint text (value is null)
 *   • "⏱ Dec 31, 2026 ×"  — solid border, with a clear button (value is set)
 *
 * Clicking the chip toggles to an inline <input type="date">.
 * CSS classes (.smd-expiry-chip-wrap, .smd-expiry-chip, .smd-expiry-date-input)
 * live in shareModal.css.
 */

import { formatExpiryDate } from '../core/formatters.js';
import { i18n } from '../core/i18n.js';

// Re-export so existing import sites keep working after the move to core/formatters.js.
export { formatExpiryDate };

/**
 * Build an interactive expiry chip.
 * @param {string|null} initialValue  YYYY-MM-DD or null
 * @param {(v: string|null) => void}  onChange  called whenever the value changes
 * @returns {HTMLElement}
 */
export function buildExpiryChip(initialValue, onChange) {
    let current = initialValue;

    const wrap = document.createElement('div');
    wrap.className = 'smd-expiry-chip-wrap';

    const chip = document.createElement('button');
    chip.type = 'button';

    const dateInput = document.createElement('input');
    dateInput.type = 'date';
    dateInput.className = 'smd-expiry-date-input hidden';

    const updateChip = () => {
        if (current) {
            chip.className = 'smd-expiry-chip smd-expiry-chip--set';
            chip.innerHTML =
                `<i class="fas fa-clock"></i> ${formatExpiryDate(current)}` +
                `<span class="smd-expiry-chip-clear" title="${i18n.t('actions.clear', 'Clear')}">×</span>`;
            chip.querySelector('.smd-expiry-chip-clear')?.addEventListener('click', (e) => {
                e.stopPropagation();
                current = null;
                onChange(null);
                updateChip();
            });
        } else {
            chip.className = 'smd-expiry-chip';
            chip.innerHTML = `<i class="fas fa-infinity"></i> ${i18n.t('share.noExpiry', 'No expiry')}`;
        }
    };

    chip.addEventListener('click', () => {
        chip.classList.add('hidden');
        if (current) dateInput.value = current;
        dateInput.classList.remove('hidden');
        dateInput.focus();
    });

    const confirm = () => {
        const val = dateInput.value || null;
        current = val;
        onChange(val);
        dateInput.classList.add('hidden');
        chip.classList.remove('hidden');
        updateChip();
    };
    dateInput.addEventListener('blur', confirm);
    dateInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            confirm();
        }
        if (e.key === 'Escape') {
            dateInput.classList.add('hidden');
            chip.classList.remove('hidden');
        }
    });

    updateChip();
    wrap.appendChild(chip);
    wrap.appendChild(dateInput);
    return wrap;
}
