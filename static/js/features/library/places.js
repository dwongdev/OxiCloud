/**
 * OxiCloud - Places (photo map)
 *
 * Renders the user's geotagged photos on a self-hosted MapLibre GL map.
 * Photos are clustered *server-side* (GET /api/photos/geo, grid aggregation),
 * so we draw one lightweight HTML marker per cluster — no glyph/sprite assets
 * and no client-side clustering needed. The vector basemap is optional: if a
 * `static/basemaps/basemap.pmtiles` is present it is read directly by the
 * browser over HTTP Range (pmtiles.js); otherwise the map falls back to a
 * plain themed background and still shows the photo clusters.
 *
 * MapLibre + pmtiles.js are heavy, so they are vendored and lazy-loaded only
 * when the Places tab is first opened.
 */

import { getCsrfHeaders } from '../../core/csrf.js';
import { i18n } from '../../core/i18n.js';
import { photosView } from './photos.js';
import { photosLightbox } from './photosLightbox.js';

/** @import {FileItem} from '../../core/types.js' */
/** @typedef {{lng: number, lat: number, count: number, sample_file_id: string}} GeoClusterItem */

const BASEMAP_URL = '/basemaps/basemap.pmtiles';

export const placesView = {
    /** @type {HTMLElement|null} */
    _container: null,
    /** @type {HTMLElement|null} */
    _subnav: null,
    /** @type {any} MapLibre Map instance */
    _map: null,
    /** @type {any[]} current cluster markers */
    _markers: [],
    /** @type {{maplibregl: any, pmtiles: any}|null} */
    _libs: null,
    /** @type {number} debounce timer for moveend refresh */
    _moveTimer: 0,
    /** @type {'moments'|'places'} */
    _activeTab: 'moments',
    /** @type {boolean|null} cached basemap availability */
    _hasBasemap: null,

    /** Auth headers (HttpOnly cookies + CSRF) */
    _headers() {
        return getCsrfHeaders();
    },

    // ── Sub-navigation (Moments | Places) ───────────────────────────
    // Lives at the top of `.content-area`; mounted while the Photos section
    // is active and torn down (hidden) when the user leaves it.

    /** Create/show the Moments|Places tab bar and the map container. */
    mountTabs() {
        const contentArea = document.querySelector('.content-area');
        if (!contentArea) return;

        if (!this._subnav) {
            const bar = document.createElement('div');
            bar.className = 'photos-subnav';
            bar.innerHTML =
                `<button class="photos-subnav-tab active" type="button" data-ptab="moments">${this._esc(i18n.t('photos.tab_moments'))}</button>` +
                `<button class="photos-subnav-tab" type="button" data-ptab="places">${this._esc(i18n.t('photos.tab_places'))}</button>`;
            bar.addEventListener('click', (e) => {
                const btn = /** @type {HTMLElement} */ (e.target).closest('[data-ptab]');
                if (btn) this._switchTab(/** @type {'moments'|'places'} */ (btn.getAttribute('data-ptab')));
            });
            contentArea.insertBefore(bar, contentArea.firstChild);
            this._subnav = bar;
        }
        this._subnav.classList.remove('hidden');

        if (!this._container) {
            const el = document.createElement('div');
            el.id = 'places-container';
            el.className = 'places-container';
            contentArea.appendChild(el);
            this._container = el;
        }

        // Always (re)enter the Photos section on the Moments tab.
        this._activeTab = 'moments';
        this._setActiveTab('moments');
        this.hide();
    },

    /** Hide the tab bar and the map (called when leaving the Photos section). */
    unmountTabs() {
        this._subnav?.classList.add('hidden');
        this.hide();
    },

    /** Hide the map container (without destroying the map). */
    hide() {
        this._container?.classList.remove('active');
    },

    /**
     * @param {'moments'|'places'} tab
     */
    _switchTab(tab) {
        if (tab === this._activeTab) return;
        this._activeTab = tab;
        this._setActiveTab(tab);
        if (tab === 'places') {
            photosView.hide();
            this._showMap();
        } else {
            this.hide();
            photosView.show();
        }
    },

    /** @param {string} tab */
    _setActiveTab(tab) {
        this._subnav?.querySelectorAll('[data-ptab]').forEach((b) => {
            b.classList.toggle('active', b.getAttribute('data-ptab') === tab);
        });
    },

    // ── Map ─────────────────────────────────────────────────────────

    /** Reveal the map container and (lazily) build the map. */
    async _showMap() {
        if (!this._container) return;
        this._container.classList.add('active');

        if (this._map) {
            this._map.resize();
            this._refreshClusters(false);
            return;
        }

        this._container.innerHTML = '<div class="places-map" id="places-map"></div>' + '<div class="places-loading"><i class="fas fa-spinner"></i></div>';
        try {
            const libs = await this._loadLibs();
            await this._initMap(libs);
        } catch (err) {
            console.error('Places map failed to load:', err);
            if (this._container) {
                this._container.innerHTML = `<div class="places-error">${this._esc(i18n.t('photos.map_error'))}</div>`;
            }
        }
    },

    /** Inject a vendored script once, resolving when it has loaded.
     * @param {string} src
     * @returns {Promise<void>}
     */
    _loadScript(src) {
        return new Promise((resolve, reject) => {
            if (document.querySelector(`script[data-vendor="${src}"]`)) {
                resolve();
                return;
            }
            const s = document.createElement('script');
            s.src = src;
            s.async = true;
            s.dataset.vendor = src;
            s.addEventListener('load', () => resolve());
            s.addEventListener('error', () => reject(new Error(`Failed to load ${src}`)));
            document.head.appendChild(s);
        });
    },

    /** Lazy-load MapLibre GL + pmtiles.js (+ MapLibre CSS) and read their globals. */
    async _loadLibs() {
        if (this._libs) return this._libs;
        if (!document.querySelector('link[data-vendor="maplibre-css"]')) {
            const l = document.createElement('link');
            l.rel = 'stylesheet';
            l.href = '/js/vendors/maplibre-gl.css';
            l.dataset.vendor = 'maplibre-css';
            document.head.appendChild(l);
        }
        await this._loadScript('/js/vendors/maplibre-gl.js');
        await this._loadScript('/js/vendors/pmtiles.js');
        const w = /** @type {any} */ (window);
        this._libs = { maplibregl: w.maplibregl, pmtiles: w.pmtiles };
        return this._libs;
    },

    /** Whether a basemap .pmtiles is available (cached after first probe). */
    async _checkBasemap() {
        if (this._hasBasemap !== null) return this._hasBasemap;
        try {
            const res = await fetch(BASEMAP_URL, { headers: { Range: 'bytes=0-0' } });
            this._hasBasemap = res.ok; // 200/206 = present, 404 = absent
        } catch {
            this._hasBasemap = false;
        }
        return this._hasBasemap;
    },

    /**
     * @param {{maplibregl: any, pmtiles: any}} libs
     */
    async _initMap({ maplibregl, pmtiles }) {
        const hasBasemap = await this._checkBasemap();
        if (hasBasemap) {
            try {
                const protocol = new pmtiles.Protocol();
                maplibregl.addProtocol('pmtiles', protocol.tile);
            } catch (e) {
                console.error('pmtiles protocol registration failed:', e);
            }
        }

        this._map = new maplibregl.Map({
            container: 'places-map',
            style: hasBasemap ? this._basemapStyle() : this._blankStyle(),
            center: [0, 25],
            zoom: 1.3,
            attributionControl: false
        });
        this._map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
        if (hasBasemap) {
            this._map.addControl(
                new maplibregl.AttributionControl({
                    customAttribution: 'Protomaps © <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a>'
                })
            );
        }

        this._map.on('load', () => {
            this._removeLoading();
            this._refreshClusters(true);
        });
        this._map.on('moveend', () => {
            clearTimeout(this._moveTimer);
            this._moveTimer = window.setTimeout(() => this._refreshClusters(false), 250);
        });
    },

    _removeLoading() {
        this._container?.querySelector('.places-loading')?.remove();
    },

    /** Fetch clusters for the current viewport and render them.
     * @param {boolean} fit Fit the map to the returned clusters (first load).
     */
    async _refreshClusters(fit) {
        if (!this._map) return;
        const b = this._map.getBounds();
        const bbox = `${b.getWest()},${b.getSouth()},${b.getEast()},${b.getNorth()}`;
        const zoom = Math.round(this._map.getZoom());
        try {
            const res = await fetch(`/api/photos/geo?bbox=${bbox}&zoom=${zoom}`, {
                credentials: 'include',
                headers: this._headers()
            });
            if (!res.ok) return;
            /** @type {GeoClusterItem[]} */
            const clusters = await res.json();
            this._renderMarkers(clusters);
            if (fit && clusters.length) this._fitTo(clusters);
        } catch (err) {
            console.error('Places geo fetch failed:', err);
        }
    },

    /** @param {GeoClusterItem[]} clusters */
    _renderMarkers(clusters) {
        for (const m of this._markers) m.remove();
        this._markers = [];
        if (!this._libs) return;
        const { maplibregl } = this._libs;

        for (const c of clusters) {
            const size = Math.round(Math.min(64, 30 + Math.log2(c.count + 1) * 6));
            const el = document.createElement('div');
            el.className = 'places-cluster';
            el.style.width = `${size}px`;
            el.style.height = `${size}px`;
            el.style.backgroundImage = `url(/api/files/${c.sample_file_id}/thumbnail/icon)`;
            if (c.count > 1) {
                el.innerHTML = `<span class="places-cluster-count">${c.count}</span>`;
            }
            el.addEventListener('click', () => this._onClusterClick(c));
            const marker = new maplibregl.Marker({ element: el }).setLngLat([c.lng, c.lat]).addTo(this._map);
            this._markers.push(marker);
        }
    },

    /** @param {GeoClusterItem} c */
    _onClusterClick(c) {
        const zoom = this._map.getZoom();
        if (c.count === 1 || zoom >= 16) {
            // Drill down to the representative photo. We only know its id, so
            // build a minimal item and let the lightbox load the rest.
            const item = /** @type {FileItem} */ (
                /** @type {any} */ ({
                    id: c.sample_file_id,
                    name: '',
                    mime_type: 'image/jpeg',
                    created_at: 0,
                    sort_date: 0,
                    size_formatted: ''
                })
            );
            photosLightbox.open([item], 0);
        } else {
            this._map.easeTo({ center: [c.lng, c.lat], zoom: Math.min(zoom + 2.5, 17) });
        }
    },

    /** @param {GeoClusterItem[]} clusters */
    _fitTo(clusters) {
        if (!this._libs) return;
        const { maplibregl } = this._libs;
        const bounds = new maplibregl.LngLatBounds();
        for (const c of clusters) bounds.extend([c.lng, c.lat]);
        if (!bounds.isEmpty()) {
            this._map.fitBounds(bounds, { padding: 64, maxZoom: 14, duration: 0 });
        }
    },

    /** @returns {boolean} */
    _isDark() {
        return document.documentElement.getAttribute('data-color-scheme') === 'dark';
    },

    /** Minimal MapLibre style: themed background only (no basemap). */
    _blankStyle() {
        return {
            version: 8,
            sources: {},
            layers: [
                {
                    id: 'bg',
                    type: 'background',
                    paint: { 'background-color': this._isDark() ? '#0f172a' : '#e8eef3' }
                }
            ]
        };
    },

    /** Label-light Protomaps vector style (no glyphs/sprites required). */
    _basemapStyle() {
        const dark = this._isDark();
        const c = dark
            ? { earth: '#1b2433', land: '#222d3d', water: '#0d1b2a', roads: '#3a4860', buildings: '#2a3547', boundary: '#475569' }
            : { earth: '#f3efe9', land: '#e9e4da', water: '#a8c8e8', roads: '#ffffff', buildings: '#e0dccf', boundary: '#c9c2b6' };
        return {
            version: 8,
            sources: {
                protomaps: {
                    type: 'vector',
                    url: `pmtiles://${BASEMAP_URL}`,
                    attribution: 'Protomaps © OpenStreetMap'
                }
            },
            layers: [
                { id: 'bg', type: 'background', paint: { 'background-color': c.earth } },
                { id: 'earth', type: 'fill', source: 'protomaps', 'source-layer': 'earth', paint: { 'fill-color': c.earth } },
                { id: 'landuse', type: 'fill', source: 'protomaps', 'source-layer': 'landuse', paint: { 'fill-color': c.land, 'fill-opacity': 0.6 } },
                { id: 'water', type: 'fill', source: 'protomaps', 'source-layer': 'water', paint: { 'fill-color': c.water } },
                { id: 'roads', type: 'line', source: 'protomaps', 'source-layer': 'roads', minzoom: 7, paint: { 'line-color': c.roads, 'line-width': 0.8 } },
                { id: 'buildings', type: 'fill', source: 'protomaps', 'source-layer': 'buildings', minzoom: 13, paint: { 'fill-color': c.buildings } },
                {
                    id: 'boundaries',
                    type: 'line',
                    source: 'protomaps',
                    'source-layer': 'boundaries',
                    paint: { 'line-color': c.boundary, 'line-width': 0.6, 'line-dasharray': [2, 2] }
                }
            ]
        };
    },

    /** @param {any} s */
    _esc(s) {
        const d = document.createElement('div');
        d.textContent = s;
        return d.innerHTML;
    }
};
