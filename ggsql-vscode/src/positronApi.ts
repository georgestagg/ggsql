/*
 * Positron API access.
 *
 * Positron's require interceptor decides which extension owns an API object
 * by matching the filesystem path of the file that called require('positron')
 * against its map of extension folders. That identity becomes the extensionId
 * on every runtime this extension registers, and Positron uses it to activate
 * the owning extension when it restores sessions after a window reload.
 *
 * The require therefore has to happen in a ggsql source file, and 'positron'
 * is marked external in esbuild.js so the call is still in out/extension.js
 * rather than inlined. Reaching the API through a global accessor instead
 * attributes it to Positron's own bootstrap file, which the interceptor
 * cannot place, and the runtime is recorded under nullExtensionDescription.
 *
 * In VS Code the module does not exist, so the require throws and the
 * extension runs without the Positron surface.
 */

import type { PositronApi } from '@posit-dev/positron';

let api: PositronApi | undefined;
let attempted = false;

/**
 * Get the Positron API, or undefined when not running in Positron.
 */
export function getPositronApi(): PositronApi | undefined {
    if (!attempted) {
        attempted = true;
        try {
            api = require('positron') as PositronApi;
        } catch {
            // Not running in Positron.
        }
    }
    return api;
}
