import { getCsrfHeaders } from '../core/csrf.js';

/** @import {FileItem} from '../core/types.js' */

// IMPORTANT: absolute paths so the dynamic import resolves correctly both in
// dev mode (native ESM, module at /js/features/thumbnail.js) and in release
// mode (IIFE bundle at /js/app.{hash}.js — relative '../vendors/…' would
// incorrectly resolve to /vendors/… instead of /js/vendors/…).
const PDFJS_LIB_URL = '/js/vendors/pdf.min.mjs';
const PDFJS_WORKER_URL = '/js/vendors/pdf.worker.min.mjs';

/**
 * Memoized import of pdf.min.mjs (in-flight or settled).
 * use any type so tsc will not scan library.
 * Reset to null on failure so a later call retries (e.g. transient offline).
 * @type {Promise<any> | null}
 */
let _pdfjsLibPromise = null;

/** True once the worker script warm-up fetch has completed successfully. */
let _pdfWorkerWarmed = false;

// TODO: do we need to add a max concurrncy ?

/**
 * Lazy-loads pdf.min.mjs on first use via dynamic import so it is never
 * bundled into the IIFE (it uses top-level await which breaks IIFE wrapping).
 * Memoizing the promise (rather than the resolved module) lets concurrent
 * callers — e.g. `preloadPdf()` racing the first real thumbnail — share a
 * single network fetch.
 * @returns {Promise<any>}
 */
function getPdfjsLib() {
    if (!_pdfjsLibPromise) {
        _pdfjsLibPromise = import(PDFJS_LIB_URL)
            .then((lib) => {
                lib.GlobalWorkerOptions.workerSrc = PDFJS_WORKER_URL;
                return lib;
            })
            .catch((err) => {
                _pdfjsLibPromise = null; // allow retry after a failed load
                throw err;
            });
    }
    return _pdfjsLibPromise;
}

export const thumbnail = {
    SUPPORTED_MIME_TYPE: [/^image\//, /^application\/pdf$/, /^video\//],
    /**
     *
     * @param {FileItem} file
     * @returns {boolean}
     */
    canHandle(file) {
        for (const re of this.SUPPORTED_MIME_TYPE) {
            if (file.mime_type.match(re)) {
                return true;
            }
        }
        return false;
    },

    /**
     * Fire-and-forget warm-up of the pdf.js stack (module + worker script).
     *
     * Called the moment a PDF row enters the DOM (see resourceIcon.js), so
     * the ~1.3 MB library downloads in the background while the user is
     * still looking at the list — instead of stalling the first thumbnail
     * render on it. Idempotent and cheap after the first call, and only
     * folders that actually contain PDFs ever pay the download.
     */
    preloadPdf() {
        // Module (≈300 KB): shares the memoized promise with real users.
        getPdfjsLib().catch(() => {
            /* transient failure — the first real use retries */
        });

        // Worker (≈1 MB): pdf.js only fetches it via `new Worker(...)` on the
        // first getDocument(), so prime the HTTP cache with a plain fetch.
        // Reading the body ensures the download completes and is cacheable.
        if (_pdfWorkerWarmed) return;
        _pdfWorkerWarmed = true;
        fetch(PDFJS_WORKER_URL)
            .then((r) => (r.ok ? r.blob() : Promise.reject(new Error(`HTTP ${r.status}`))))
            .catch(() => {
                _pdfWorkerWarmed = false; // allow retry on a later sighting
            });
    },

    // TODO: use these informations from server ?
    SIZES: {
        icon: { width: 150, height: 150 },
        preview: { width: 300, height: 300 },
        large: { width: 900, height: 800 }
    },

    // note: server moved to jpeg q=80 for images
    // FORMAT: 'image/webp',
    // QUALITY: 0.85,
    FORMAT: 'image/jpeg',
    QUALITY: 0.8,

    /**
     * @typedef {Object} Size
     * @property {number} width
     * @property {number} height
     */

    /**
     *
     * @param {number} srcWidth
     * @param {number} srcHeight
     * @param {number} targetWidth
     * @param {number} targetHeight
     * @returns {Size}
     *
     * @private
     */
    _computeSize(srcWidth, srcHeight, targetWidth, targetHeight) {
        const srcRatio = srcWidth / srcHeight;
        const targetRatio = targetWidth / targetHeight;
        if (srcRatio > targetRatio) {
            return { width: targetWidth, height: Math.round(targetWidth / srcRatio) };
        } else {
            return { width: Math.round(targetHeight * srcRatio), height: targetHeight };
        }
    },

    /**
     *
     * @param {ImageBitmap} bitmap
     * @param {number} targetWidth
     * @param {number} targetHeight
     * @param {ImageEncodeOptions} imageEncodeOptions
     * @returns {Promise<Blob>}
     *
     * @private
     */
    _bitmapToBlob(bitmap, targetWidth, targetHeight, imageEncodeOptions) {
        const { width, height } = this._computeSize(bitmap.width, bitmap.height, targetWidth, targetHeight);
        const canvas = new OffscreenCanvas(width, height);
        canvas.getContext('2d')?.drawImage(bitmap, 0, 0, width, height);
        return canvas.convertToBlob(imageEncodeOptions);
    },

    /**
     *
     * @param {Blob} blob
     * @returns {Promise<any>}
     *
     * @private
     */
    _blobToDataUrl(blob) {
        return new Promise((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result);
            reader.onerror = reject;
            reader.readAsDataURL(blob);
        });
    },

    /**
     *
     * @param {FileItem} file
     * @param {string} source
     * @returns {Promise<ImageBitmap>}
     *
     * @private
     */
    async _sourceToBitmap(file, source) {
        // FIXME: more efficient to use mimetype
        if (file.mime_type.startsWith('image/')) {
            const response = await fetch(source);
            if (!response.ok) throw new Error(`failed to fetch: ${response.status}`);
            const blob = await response.blob();
            return createImageBitmap(blob);
        }

        if (file.mime_type === 'application/pdf') {
            const pdfjsLib = await getPdfjsLib();
            const pdf = await pdfjsLib.getDocument(source).promise;
            const page = await pdf.getPage(1);
            const viewport = page.getViewport({ scale: 1 });
            const canvas = document.createElement('canvas');
            canvas.width = viewport.width;
            canvas.height = viewport.height;
            await page.render({ canvasContext: canvas.getContext('2d'), viewport }).promise;
            return createImageBitmap(canvas);
        }

        if (file.mime_type.startsWith('video/')) {
            return new Promise((resolve, reject) => {
                const video = document.createElement('video');
                video.src = source;
                video.muted = true;
                video.preload = 'metadata';
                video.onloadedmetadata = () => {
                    // seek to 1/3 of video to take snapshot
                    video.currentTime = video.duration / 3;
                };
                video.onseeked = async () => {
                    const bitmap = await createImageBitmap(video);
                    video.pause();
                    video.removeAttribute('src'); // hack to close network connection
                    video.load();
                    resolve(bitmap);
                };
                video.onerror = reject;
            });
        }

        throw new Error(`unsupported mime type: ${file.mime_type} for file ${file.name}`);
    },

    /**
     * generateThumbnail and update image
     *
     * @param {FileItem} file the source of the image
     * @param {((dataURL: string) => void) | null} [onIconGenerated] the callback once thumbnail is generated
     * @param {((dataURL: string) => void) | null} [onPreviewGenerated] the callback once thumbnail is generated
     *
     * @private
     */
    async _generate(file, onIconGenerated, onPreviewGenerated) {
        const source = `${window.location.origin}/api/files/${file.id}`;

        const bitmap = await this._sourceToBitmap(file, source);

        const [iconBlob, previewBlob, largeBlob] = await Promise.all(
            Object.values(this.SIZES).map(({ width, height }) => this._bitmapToBlob(bitmap, width, height, { type: this.FORMAT, quality: this.QUALITY }))
        );

        if (onIconGenerated) {
            onIconGenerated(await this._blobToDataUrl(iconBlob));
        }

        if (onPreviewGenerated) {
            onPreviewGenerated(await this._blobToDataUrl(previewBlob));
        }

        await Promise.all(
            [
                ['icon', iconBlob],
                ['preview', previewBlob],
                ['large', largeBlob]
            ].map(([size, blob]) =>
                fetch(`${window.location.origin}/api/files/${file.id}/thumbnail/${size}`, {
                    method: 'PUT',
                    headers: { ...getCsrfHeaders(), 'Content-Type': this.FORMAT },
                    body: blob
                }).then((r) => console.log(`uploaded ${size} thumbnail of ${file.name}: ${r.status}`))
            )
        );
    },

    MAX_CONCURRENT: 3,
    _activeGenerates: 0,
    /** @type {Array<(resolve: any) => void>} */
    _generateQueue: [],

    /**
     * Concurrency-limited wrapper around generate().
     * At most MAX_CONCURRENT generations run simultaneously; excess calls are
     * queued and resume automatically as slots free up.
     *
     * @param {FileItem} file
     * @param {((dataURL: string) => void) | null} [onIconGenerated]
     * @param {((dataURL: string) => void) | null} [onPreviewGenerated]
     * @returns {Promise<void>}
     */
    async queueGenerate(file, onIconGenerated, onPreviewGenerated) {
        if (this._activeGenerates >= this.MAX_CONCURRENT) {
            await new Promise((resolve) => this._generateQueue.push(resolve));
        }
        this._activeGenerates++;
        try {
            await this._generate(file, onIconGenerated, onPreviewGenerated);
        } catch (err) {
            if (err instanceof Event && 'error' in err.target) {
                console.warn(`generation of thumbnail for ${file.name} failed: `, err.target.error);
            } else if (err instanceof Error) {
                console.warn(`generation of thumbnail for ${file.name} failed: `, err.message);
            } else {
                console.warn(`generation of thumbnail for ${file.name} failed: `, err);
            }
        } finally {
            this._activeGenerates--;
            if (this._generateQueue.length > 0) {
                this._generateQueue.shift()();
            }
        }
    }
};
