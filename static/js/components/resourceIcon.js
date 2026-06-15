/**
 * resourceIcon — shared resource icon builder.
 *
 * Returns a `.file-icon` element identical to the one in resourceList:
 *   • Folders: `.file-icon.folder-icon` with CSS tab (no visible <i>)
 *   • Files:   `.file-icon.{specialClass}` + optional thumbnail <img> + <i>
 *
 * CSS lives in fileType.css (folder/file type colours) and resourceList.css
 * (base size in grid/list context). Consumer views add their own size overrides.
 */

import { thumbnail } from '../features/thumbnail.js';

/** @import {FileItem, FolderItem} from '../core/types.js' */

/**
 * @param {FileItem|FolderItem} item
 * @param {'file'|'folder'} resourceType
 * @returns {HTMLElement}
 */
function buildResourceIcon(item, resourceType) {
    const el = document.createElement('div');

    if (resourceType === 'folder') {
        el.className = 'file-icon folder-icon';
        const i = document.createElement('i');
        i.className = 'fas fa-folder';
        el.appendChild(i);
        return el;
    }

    const file = /** @type {FileItem} */ (item);
    const iconClass = file.icon_class || 'fas fa-file';
    const iconSpecialClass = file.icon_special_class || '';
    el.className = `file-icon${iconSpecialClass ? ` ${iconSpecialClass}` : ''}`;

    const canThumbnail = thumbnail?.canHandle(file) ?? false;
    if (canThumbnail) {
        // A PDF just entered the list: warm up the pdf.js stack (~1.3 MB)
        // in the background now, so a thumbnail cache-miss below doesn't
        // stall its first render on the library download. Idempotent.
        if (file.mime_type === 'application/pdf') {
            thumbnail.preloadPdf();
        }

        const img = document.createElement('img');
        img.className = 'file-thumb';
        img.src = `/api/files/${file.id}/thumbnail/preview`;
        img.loading = 'lazy';
        img.alt = '';
        img.addEventListener('error', () => {
            img.classList.add('hidden');
            thumbnail?.queueGenerate(file, (dataUrl) => {
                img.src = dataUrl;
                img.classList.remove('hidden');
            });
        });
        el.appendChild(img);
    }

    const i = document.createElement('i');
    i.className = iconClass;
    el.appendChild(i);

    return el;
}

export { buildResourceIcon };
