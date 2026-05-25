/**
 * @import {Grant, ResourceTypeEnum, SharedWithMeResponse} from '../core/types.js'
 */

import { getCsrfHeaders } from '../core/csrf.js';

const grants = {
    /** @type {Record<String, Record<String, Grant[]>>} */
    outgoingGrants: {},

    /** @type {Record<String, Record<String, Grant[]>>} */
    incomingGrants: {},

    async fetchOutgoingGrants() {
        const response = await fetch('/api/grants/outgoing');

        if (!response.ok) {
            console.error(`error ${response.status} while fetching /api/grants/outgoing`);
            return;
        }

        /** @type {Grant[]} */
        const outgoingGrants = await response.json();

        // Reset and rebuild cache
        this.outgoingGrants = {};

        // store grants by type, then by id
        outgoingGrants.forEach((grant) => {
            this.outgoingGrants[grant.resource.type] ??= {};
            this.outgoingGrants[grant.resource.type][grant.resource.id] ??= [];
            this.outgoingGrants[grant.resource.type][grant.resource.id].push(grant);
        });
    },

    /**
     * get grant for a resource
     * @param {ResourceTypeEnum} resourceType
     * @param {String} id
     * @returns {Grant[] | null}
     */
    getOutgoingGrantsFor(resourceType, id) {
        try {
            return this.outgoingGrants[resourceType][id] ?? [];
        } catch {
            return [];
        }
    },

    async fetchIncomingGrants() {
        const response = await fetch('/api/grants/incoming');

        if (!response.ok) {
            console.error(`error ${response.status} while fetching /api/grants/incoming`);
            return;
        }

        /** @type {Grant[]} */
        const incomingGrants = await response.json();

        // store grants by type, then by id
        incomingGrants.forEach((grant) => {
            this.incomingGrants[grant.resource.type] ??= {};
            this.incomingGrants[grant.resource.type][grant.resource.id] ??= [];
            this.incomingGrants[grant.resource.type][grant.resource.id].push(grant);
        });
    },

    /**
     * get grant for a resource
     * @param {ResourceTypeEnum} resourceType
     * @param {String} id
     * @returns {Grant[] | null}
     */
    getIncomingGrantsFor(resourceType, id) {
        try {
            return this.incomingGrants[resourceType][id] ?? [];
        } catch {
            return [];
        }
    },

    /**
     * Fetch a cursor-paginated list of resources shared with the current user,
     * with full file / folder metadata resolved server-side.
     *
     * @param {object}            [opts]
     * @param {ResourceTypeEnum[]} [opts.resourceTypes] - Resource types to include (default: ['file','folder']).
     * @param {number}             [opts.limit]         - Max items per page (1–200, default 50).
     * @param {string}             [opts.cursor]        - Opaque cursor from a previous call; omit for first page.
     * @returns {Promise<SharedWithMeResponse>}
     */
    async fetchSharedWithMe({ resourceTypes = ['file', 'folder'], limit = 50, cursor } = {}) {
        const params = new URLSearchParams({
            limit: String(limit),
            resource_types: resourceTypes.join(',')
        });
        if (cursor) params.set('cursor', cursor);

        const response = await fetch(`/api/grants/incoming/resources?${params}`);

        if (!response.ok) {
            throw new Error(`Failed to fetch shared-with-me items: HTTP ${response.status}`);
        }

        return response.json();
    },

    /**
     * Fetch all grants on a specific resource (for the "Manage sharing" panel).
     * Refreshes the outgoingGrants cache for this resource.
     *
     * @param {ResourceTypeEnum} resourceType
     * @param {string}           resourceId
     * @returns {Promise<Grant[]>}
     */
    async fetchGrantsForResource(resourceType, resourceId) {
        const params = new URLSearchParams({ resource_type: resourceType, resource_id: resourceId });
        const response = await fetch(`/api/grants?${params}`, { credentials: 'same-origin' });

        if (!response.ok) {
            throw new Error(`fetchGrantsForResource: HTTP ${response.status}`);
        }

        /** @type {Grant[]} */
        const result = await response.json();

        // Refresh the outgoing cache for this resource
        this.outgoingGrants[resourceType] ??= {};
        this.outgoingGrants[resourceType][resourceId] = result;

        return result;
    },

    /**
     * Create a new grant.
     * Body mirrors `CreateGrantDto`: `{ subject, resource, role }` OR `{ subject, resource, permissions }`.
     *
     * @param {Object} dto - CreateGrantDto shape
     * @returns {Promise<Grant[]>}
     */
    async createGrant(dto) {
        const response = await fetch('/api/grants', {
            method: 'POST',
            credentials: 'same-origin',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify(dto)
        });

        if (!response.ok) {
            const body = await response.json().catch(() => ({}));
            throw new Error(body.error || `createGrant: HTTP ${response.status}`);
        }

        return response.json();
    },

    /**
     * Reconcile a subject's role on a resource (replaces all their permissions).
     * Body mirrors `UpdateRoleDto`: `{ subject, resource, role }`.
     *
     * @param {Object} dto - UpdateRoleDto shape
     * @returns {Promise<Grant[]>}
     */
    async updateRole(dto) {
        const response = await fetch('/api/grants/role', {
            method: 'PUT',
            credentials: 'same-origin',
            headers: { 'Content-Type': 'application/json', ...getCsrfHeaders() },
            body: JSON.stringify(dto)
        });

        if (!response.ok) {
            const body = await response.json().catch(() => ({}));
            throw new Error(body.error || `updateRole: HTTP ${response.status}`);
        }

        return response.json();
    },

    /**
     * Revoke a single grant by its UUID.
     *
     * @param {string} grantId
     * @returns {Promise<void>}
     */
    async revokeGrant(grantId) {
        const response = await fetch(`/api/grants/${encodeURIComponent(grantId)}`, {
            method: 'DELETE',
            credentials: 'same-origin',
            headers: getCsrfHeaders()
        });

        if (!response.ok) {
            throw new Error(`revokeGrant: HTTP ${response.status}`);
        }
    }
};

export { grants };
