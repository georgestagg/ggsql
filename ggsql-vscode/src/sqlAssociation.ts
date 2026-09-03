/*
 * Points users at `files.associations` when they open a plain `.sql` file.
 *
 * Running SQL through the ggsql kernel already works without any association
 * (see `languages.ts`); mapping the files additionally gives them ggsql syntax
 * highlighting and the ggsql file icon. That is a preference, not a
 * prerequisite, so the extension only surfaces the option and lets the user
 * make the change. It deliberately does not write `files.associations` itself:
 * mapping `*.sql` globally rewrites how every `.sql` file in every workspace is
 * treated, which is not a decision an extension should take on someone's behalf.
 *
 * `files.associations` is the right mechanism for users who do want it, because
 * user-configured associations outrank every extension-contributed one and so
 * win deterministically even with other SQL extensions installed.
 *
 * The notice appears on each `.sql` file opened until dismissed for good, and
 * never when `*.sql` is already mapped.
 */

import * as vscode from 'vscode';
import { SQL_LANGUAGE_ID, sqlFilesEnabled } from './languages';
import { log } from './extension';

/** Set once the user has actively dismissed the notice for good. */
const PROMPTED_KEY = 'ggsql.sqlAssociationPrompted';

const SQL_GLOB = '*.sql';

const SETTINGS_QUERY = 'files.associations';

/**
 * True while a notice is on screen awaiting an answer.
 *
 * The notice is shown every time a `.sql` file is opened, because an Info
 * notification with buttons is not sticky and auto-hides after 10 seconds
 * (`notificationsToasts.ts`), and extensions cannot opt out of that: a
 * show-once notice is trivially missed. Letting it age out is therefore not
 * treated as an answer, only "Don't show again" is.
 *
 * This guard keeps that from becoming a pile-up. Opening a folder full of
 * `.sql` files, or another extension opening them in the background, would
 * otherwise queue one notice per file; instead the first is shown and the rest
 * are skipped while it is still pending.
 */
let noticeVisible = false;

/**
 * True when `*.sql` is already mapped at any level.
 *
 * Reads the *effective* value on purpose: if a default, a workspace or the user
 * maps `*.sql` to anything, there is nothing to tell them about.
 */
function hasSqlAssociation(): boolean {
	const associations = vscode.workspace
		.getConfiguration('files')
		.get<Record<string, string>>('associations', {});
	return SQL_GLOB in associations;
}

/*
 * Notification text is parsed by `parseLinkedText`, which understands markdown
 * links and nothing else: no code spans, no emphasis. Backticks would render
 * literally, so the mapping is spelled out in prose and the setting name is a
 * command link. Only https, command and file hrefs are permitted, and the href
 * may contain no spaces or `)`.
 *
 * `workbench.action.openSettings` takes its query as a plain string argument,
 * and the opener JSON-parses the URI query then wraps a non-array in an array,
 * so an encoded bare string is enough.
 */
const SETTINGS_LINK =
	`[Files: Associations](command:workbench.action.openSettings?%22files.associations%22)`;

const MESSAGE =
	'You can run this .sql file in the ggsql console. For ggsql syntax highlighting in '
	+ `.sql files as well, map "${SQL_GLOB}" to "ggsql" in ${SETTINGS_LINK}.`;

async function showNotice(context: vscode.ExtensionContext): Promise<void> {
	noticeVisible = true;
	const showSetting = 'Show Setting';
	const dontShow = 'Don\'t show again';
	let choice: string | undefined;
	try {
		choice = await vscode.window.showInformationMessage(MESSAGE, showSetting, dontShow);
	} finally {
		noticeVisible = false;
	}

	if (choice === showSetting) {
		await vscode.commands.executeCommand('workbench.action.openSettings', SETTINGS_QUERY);
	}
	// Only an explicit "don't show again" is persisted. Anything else, including
	// the toast ageing out unseen, leaves the notice free to appear again.
	if (choice === dontShow) {
		await context.globalState.update(PROMPTED_KEY, true);
	}
}

/**
 * Clears the persisted dismissal so the notice can appear again.
 *
 * Only reachable state to undo is "Don't show again", which otherwise has no
 * route back short of editing the global storage database.
 */
async function resetNotice(context: vscode.ExtensionContext): Promise<void> {
	await context.globalState.update(PROMPTED_KEY, undefined);
	log('Reset the .sql association notice');

	if (hasSqlAssociation()) {
		// An existing mapping makes `.sql` files open as `ggsql`, so the notice
		// still will not fire. Say so rather than appearing to have done nothing.
		void vscode.window.showInformationMessage(
			`Notice reset, but "${SQL_GLOB}" is still mapped in ${SETTINGS_QUERY}, so .sql `
			+ 'files open as ggsql and the notice stays hidden. Remove that mapping to see it.',
		);
		return;
	}

	void vscode.window.showInformationMessage(
		'ggsql will mention the .sql association again next time you open a .sql file.',
	);
}

/**
 * Mentions the association whenever a plain `.sql` document is opened.
 *
 * Registered regardless of whether Positron is present: the association affects
 * syntax highlighting, which is useful even where the kernel cannot run.
 */
export function activateSqlAssociationPrompt(context: vscode.ExtensionContext): void {
	context.subscriptions.push(
		vscode.commands.registerCommand(
			'ggsql.resetSqlAssociationPrompt',
			() => resetNotice(context),
		),
	);

	const maybeNotify = (document: vscode.TextDocument | undefined): void => {
		if (!document || document.languageId !== SQL_LANGUAGE_ID) {
			return;
		}
		// Only real files on disk; skip untitled buffers, diffs and output panes.
		if (document.uri.scheme !== 'file') {
			return;
		}
		if (noticeVisible || context.globalState.get<boolean>(PROMPTED_KEY)) {
			return;
		}
		if (hasSqlAssociation()) {
			return;
		}
		if (!sqlFilesEnabled()) {
			return;
		}
		void showNotice(context);
	};

	maybeNotify(vscode.window.activeTextEditor?.document);

	context.subscriptions.push(
		vscode.workspace.onDidOpenTextDocument(maybeNotify),
	);
}
