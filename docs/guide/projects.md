# Projects and the Project Manager

**[Resolve-style] [Fork]** · the home screen that lists your projects (RESOLVE-UI-PLAN RS2)

## What the Project Manager is

> id: projects.what | does: a full-window screen of project tiles to open, make and manage projects | ui: pageBar.home | cmd: file.projectManager, projects.show | resolve: Project Manager | premiere: Home screen | when: starting work; switching projects | see: projects.where

**[Resolve-style]**

The **Project Manager** is a full-window screen that shows your projects as picture tiles. You
pick one to work on, or make a new one, instead of starting in an empty project. In the Resolve
workflow it is the first thing you see when Revolution starts. While it is open nothing plays:
opening it stops playback, and Play waits until you close it.

If you know Resolve: this is its Project Manager, with the same layout (projects as tiles, New
Project as the first tile, Open / Close at the bottom right). Differences are listed at the end
of this page. If you know Premiere: it plays the part of Premiere's Home screen and recent
projects list.

## Where projects live

> id: projects.where | does: each project is a .fcproj file in a projects folder, not a database | ui: settings.general.projectsFolder | cmd: app.settings.general | resolve: project database | premiere: .prproj files | when: where projects are saved by default; backing up, syncing or moving projects | see: projects.what

**[Fork]**

Resolve keeps projects inside a database. Revolution keeps each project in its own `.fcproj`
file, so the Project Manager shows a **projects folder**:

- By default it is `~/Videos/FilmCraft Projects`. It is made the first time you create a project.
- Folders inside it are the Project Manager's folders.
- Projects you opened from somewhere else are listed under **Other locations** (at the top
  level). A project that was moved or deleted shows greyed as **Missing**.

To use another folder: Edit ▸ Preferences ▸ **General…** (`app.settings.general`), then
**Projects folder** ({ui:settings.general.projectsFolder}); leave it empty for the default.

## Opening the Project Manager

> id: projects.opening | does: when the Project Manager appears and how to open it | ui: pageBar.home, settings.general.startupShow | cmd: file.projectManager | resolve: Project Manager at launch | premiere: Home screen at launch | when: choosing what shows at startup; getting back to the project list from inside a project | see: projects.what

**[Resolve-style]**

- At launch, in the Resolve workflow (unless a project was opened from the command line or a
  file manager).
- **File ▸ Project Manager** in the Resolve workflow, **File ▸ Projects…** in the Premiere
  workflow (command `projects.show`).
- **Shift+1** with the DaVinci Resolve Compatible keyboard.
- The **Home** button at the right end of the page bar ({ui:pageBar.home}).

What Revolution shows at launch is set in Edit ▸ Preferences ▸ General ▸ **At startup show**
({ui:settings.general.startupShow}): **Project Manager**, **Last project** or **New empty
project**. **Workflow default** means the Project Manager in the Resolve workflow, and Premiere's
own **At Startup** setting in the Premiere workflow.

## Make a new project

> id: projects.new | does: makes a new project or folder from the Project Manager | ui: projects.tile.new, projects.new, projects.newFolder, projects.dialog.* | cmd: projects.new, projects.newFolder | resolve: New Project | premiere: New Project | when: starting something new | see: projects.open

**[Resolve-style]**

1. Click the **New Project** tile ({ui:projects.tile.new}) or the **New Project** button at the
   bottom right ({ui:projects.new}).
2. Type a name in the box ({ui:projects.dialog.name}).
3. Click **Create** ({ui:projects.dialog.ok}) or press Enter (command `projects.new`, parameter
   `name`).

The project is saved as `<name>.fcproj` in the folder you are looking at, opened, and you land
on the **Edit** page. A name that is already used in that folder is refused; nothing is
overwritten. If the project you had open has unsaved changes, Revolution asks first: **Save**,
**Don't Save** or **Cancel**.

## Open a project

> id: projects.open | does: opens a project from its tile or the Open button | ui: projects.open | cmd: projects.open | resolve: open project | premiere: Open Project | when: getting back to work | see: projects.find

**[Resolve-style]**

1. Double-click its tile, or click it and press Enter, or click **Open** ({ui:projects.open})
   (command `projects.open`, parameter `path`).
2. The project opens on the page it was last saved on (Edit when that isn't known).

To go back to the project you had open without opening another, click **Close**
({ui:projects.close}) or press Esc. With no project open, the Project Manager stays.

Each tile shows the frame that was under the playhead when the project was last saved. A project
saved before this feature, or by another build, shows a film icon until you save it once here.

## Find, sort and view

> id: projects.find | does: search, sort, grid or list view and tile size | ui: projects.search, projects.sort, projects.view.*, projects.thumbSize | cmd: projects.list | resolve: Project Manager sort and search | premiere: Home screen recent list | when: many projects | see: projects.manage

**[Resolve-style]**

The row under the title has, from the left:

- **Sidebar** ({ui:projects.sidebar}): the projects folder and its folders on the left.
- The path: **Projects** ({ui:projects.crumb.root}) and the folders you are in; click one to go
  back up. Double-click a folder tile to go into it.

and from the right:

- **Search** ({ui:projects.search}): shows only names containing what you type.
- **Grid** ({ui:projects.view.grid}) and **list** ({ui:projects.view.list}) views. The list shows
  name, date modified, date created, resolution, frame rate and the number of timelines.
- **Info** ({ui:projects.info}): the selected project's details on the right.
- **Sort** ({ui:projects.sort}): by name, date modified (newest first) or date created; **Reverse
  order** turns it round.
- **Thumbnail size** ({ui:projects.thumbSize}).
- **New folder** ({ui:projects.newFolder}, command `projects.newFolder`).

(Command `projects.list` returns the same listing for agents: parameters `folder`, `sort`,
`descending`, `search`.)

## Rename, duplicate, move to the trash

> id: projects.manage | does: renames, duplicates, locates or trashes a project from its menu | cmd: projects.rename, projects.duplicate, projects.trash, projects.reveal, projects.locate, projects.removeFromList | resolve: Project Manager context menu | premiere: — (Finder / file manager) | when: tidying projects | see: projects.export-import

**[Resolve-style] [Fork]**

Right-click a tile for its menu:

- **Rename…** (`projects.rename`): renames the file (or folder). The project that is open can't
  be renamed here; use File ▸ Save As.
- **Duplicate** (`projects.duplicate`): a copy named "<name> copy" next to it.
- **Reveal in File Manager** (`projects.reveal`).
- **Export…** and **Move to Trash…** (below).
- For projects under Other locations: **Remove from List** (`projects.removeFromList`) forgets
  it without touching the file. A missing project offers **Locate…** (`projects.locate`).

**Move to Trash…** (`projects.trash`) asks first, then moves the file (or a folder inside the
projects folder) to the system trash, where you can restore it. Revolution never deletes a
project permanently. The project that is open can't be moved to the trash.

## Export and import

> id: projects.export-import | does: exports a project as a file to move or archive it, and imports one | ui: projects.export, projects.import | cmd: projects.export, projects.import | resolve: Export / Import Project (.drp) | premiere: Project Manager copy, .prproj | when: moving to another computer, archiving | see: projects.where

**[Fork]**

1. Select a project and click **Export** ({ui:projects.export}), then choose a folder
   (command `projects.export`, parameters `path` and `destination`).
2. Revolution writes `<name> archive` there: the project plus copies of all its media, using the
   same copier as Media Management (Premiere: Project Manager ▸ Collect Files). It runs in the
   background; the status bar shows its progress.
3. Copy the archive folder to the other computer, click **Import** ({ui:projects.import}) there
   and choose the `.fcproj` file inside it: a project is one `.fcproj` file, not an entry in a
   database. Import copies only that file into your projects folder, so if clips show as offline,
   use **Link Media…** (`media.linkMedia`) and point it at the archive's media.

**Import** ({ui:projects.import}, command `projects.import`) copies a `.fcproj` file into the
folder you are looking at (a second copy gets a number, nothing is overwritten). Agents can also
import a whole exported archive folder by passing its path.

## Where it differs from Resolve

> id: projects.resolve | does: how projects differ from Resolve's database | resolve: project database, .drp | when: you come from Resolve | see: projects.where

**[Resolve-style]**

- Projects are files in a folder, not entries in a database: you can copy, back up or sync them
  like any other file, and Revolution lists what it finds there.
- Only the **Local** tab: Resolve's Network and Cloud project libraries are not planned.
- Export / Import use folders (a project plus its media), not Resolve's `.drp` / `.dra` files;
  `.zip` archives are not made yet.
- The collect / consolidate tool that Premiere calls Project Manager is **File ▸ Media
  Management…** in the Resolve workflow (same command, `file.projectManager`).
