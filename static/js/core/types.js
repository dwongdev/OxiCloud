/**
 * @typedef {'file' | 'folder'} ItemTypeEnum
 */

// FIXME to simplify
/**
 * @typedef {Object} LightItem
 * @property {string} id
 * @property {string} name
 * @property {ItemTypeEnum} type
 * @property {string} parentId
 */

//FIXME: rename into FolderItem
/**
 * @typedef {Object} FolderItem
 * @property {string} category (folder)
 * @property {number} created_at - timestamp
 * @property {string} icon_class
 * @property {string} icon_special_class
 * @property {string} id the uniq id of the folder
 * @property {boolean} is_root
 * @property {number} modified_at
 * @property {string} name
 * @property {string} owner_id
 * @property {string|null} parent_id the folder parent (null if is_root)
 * @property {string} path the full path
 */

//FIXME: rename into FileItem
/**
 * @typedef {Object} FileItem
 * @property {string} category
 * @property {number} created_at - timestamp
 * @property {string} icon_class
 * @property {string} icon_special_class
 * @property {string} id the uniq id of the folder
 * @property {string} mime_type
 * @property {number} modified_at - timestamp
 * @property {string} name
 * @property {string} owner_id
 * @property {string} folder_id the folder parent
 * @property {string} path the full path
 * @property {number} size
 * @property {string} size_formatted
 * @property {number} sort_date
 */

/**
 * @typedef {Object} SharePermissions
 * @property {boolean} read
 * @property {boolean} reshare
 * @property {boolean} write
 */

/**
 * @typedef {Object} ShareItem
 * @property {number} access_count
 * @property {number} created_at - timestamp
 * @property {String} created_by
 * @property {number} expires_at - timestamp
 * @property {boolean} has_password
 * @property {string} id
 * @property {string} item_id
 * @property {string} item_name
 * @property {ItemTypeEnum} item_type
 * @property {SharePermissions} permissions
 * @property {string | null} token
 * @property {string} url
 */

/**
 * @typedef {Object} CreateShare
 * @property {string} item_id
 * @property {string|null} [item_name]
 * @property {ItemTypeEnum} item_type
 * @property {string|null} password
 * @property {number|null} expires_at - timestamp
 * @property {SharePermissions|null} permissions
 */

/**
 * @typedef {Object} UpdateShare
 * @property {string|null} password
 * @property {number|null} expires_at - timestamp
 * @property {SharePermissions|null} permissions
 */

/**
 * @typedef {Object} FavoriteItem
 * @property {string} id
 * @property {string} user_id
 * @property {string} item_id /// ID of the favorited item (file or folder)
 * @property {ItemTypeEnum} item_type
 * @property {number} created_at
 * @property {string|null} item_name: null if folder
 * @property {number|null} item_size null if folder
 * @property {string|null} item_mime_type if file
 * @property {string|null} parent_id
 * @property {number|null} modified_at: Option<DateTime<Utc>>,
 * @property {String} item_path Full human-readable path (e.g. "Documents/Work" for a folder, "Documents/Work/report.pdf" for a file)
 * @property {String} icon_class
 * @property {String} icon_special_class
 * @property {String} category
 * @property {String} size_formatted
 * @property {string|null} owner_id UUID of the file/folder's actual owner
 */

/**
 * @typedef {Object} RecentItem
 * @property {string} id
 * @property {string} user_id
 * @property {string} item_id /// ID of the favorited item (file or folder)
 * @property {ItemTypeEnum} item_type
 * @property {number} accessed_at
 * @property {string|null} item_name: null if folder
 * @property {number|null} item_size null if folder
 * @property {string|null} item_mime_type if file
 * @property {string|null} parent_id
 * @property {String} item_path Full human-readable path (e.g. "Documents/Work" for a folder, "Documents/Work/report.pdf" for a file)
 * @property {String} icon_class
 * @property {String} icon_special_class
 * @property {String} category
 * @property {String} size_formatted
 */

/**
 * @typedef {Object} TrashItem
 * @property {string} id
 * @property {string} original_id
 * @property {ItemTypeEnum} item_type
 * @property {string} name
 * @property {string} original_path - timestamp
 * @property {number} trashed_at
 * @property {number} days_until_deletion
 * @property {string} category
 * @property {string} icon_class
 * @property {string} icon_special_class
 */

/**
 * @typedef {Object} User
 * @property {string} id
 * @property {string} username
 * @property {string} email
 * @property {string} role
 * @property {number} storage_quota_bytes
 * @property {number} storage_used_bytes
 * @property {number} created_at
 * @property {number} updated_at
 * @property {number} last_login_at
 * @property {boolean} active
 * @property {string}  auth_provider
 */

/**
 * @typedef {Object} AuthResponse
 * @property {User} user
 * @property {String} access_token
 * @property {String} refresh_token
 * @property {String} token_type
 * @property {number} expires_in
 */

/**
 * @typedef {'user' | 'admin'} RoleEnum
 */

/**
 * @typedef {"relevance" | "name" | "name_desc" | "date" | "date_desc" | "size" | "size_desc"} SortByEnnum
 */

/**
 * @typedef {Object} SearchCriteria
 * @property {SortByEnnum} sort_by
 * @property {boolean} recursive
 * @property {number} limit
 * @property {number} offset
 *
 * @property {String} [name_contains]
 * @property {String[]} [file_types] pdf, jpg, ...
 * @property {String} [folder_id]
 *
 *
 * @property {number} [min_size]
 * @property {number} [max_size]
 *
 * @property {number} [created_before]
 * @property {number} [created_after]
 *
 * @property {number} [modified_before]
 * @property {number} [modified_after]
 */

/**
 * @typedef {Object} SearchResults
 * FIXME: is in fact Vec<SearchFileResultDto>,
 * @property {FileItem[]} files
 * FIXME: is infact Vec<SearchFolderResultDto>,
 * @property {FolderItem[]} folders:
 * @property {number | null} total_count
 * @property {number} limit
 * @property {number} offset
 * @property {boolean} has_more
 * @property {number} query_time_ms
 * @property {string} sort_by
 */

/**
 * @typedef {Object} Playlist
 * @property {String} id
 * @property {String} name
 * @property {String | null} description
 * @property {String} owner_id
 * @property {boolean} is_public
 * @property {String | null} cover_file_id
 * @property {number} track_count
 * @property {number} total_duration_secs
 * @property {number} created_at
 * @property {number} updated_at
 */

/**
 * @typedef {Object} PlaylistItem
 * @property {String} id
 * @property {String} playlist_id
 * @property {String} file_id
 * @property {number} position
 * @property {number} added_at
 * @property {String|null} file_name
 * @property {number|null} file_size
 * @property {String|null} mime_type
 * @property {String|null} title
 * @property {String|null} artist
 * @property {String|null} album
 * @property {number|null} duration_secs
 */

/**
 * @typedef {Object} Musicshare
 * @property {String} user_id
 * @property {boolean|null} can_write
 */

/**
 * @typedef {Object} FileMetadata
 * @property {String} file_id
 * @property {number} captured_at
 * @property {number|null} latitude
 * @property {number|null} longitude
 * @property {String|null} camera_make
 * @property {String|null} camera_model
 * @property {number|null} orientation
 * @property {number|null} width
 * @property {number|null} height
 */

// ------------------- grants

/**
 * @typedef {'read'|'create'|'share'|'comment'|'delete'|'update'} PermissionTypeEnum
 */

/**
 * @typedef {'folder'|'file'} ResourceTypeEnum
 */

/**
 * @typedef {Object} Resource
 * @property {ResourceTypeEnum} type
 * @property {String} id
 */

/**
 * @typedef {'user'|'group'|'token'|'external'} SubjectTypeEnum
 */

/**
 * @typedef {Object} Subject
 * @property {SubjectTypeEnum} type
 * @property {String} id
 */

/**
 * @typedef {Object} Grant
 * @property {string} id
 * @property {number} granted_at
 * @property {string} granted_by
 * @property {Subject} subject
 * @property {PermissionTypeEnum} permission
 * @property {Resource} resource
 */

/**
 * Roles: `viewer`, `commenter`, `editor`, `manager`, `admin`
 */

/**
 * One item returned by `GET /api/grants/incoming/resources`.
 * `resource_type` discriminates the shape of `resource`.
 * @typedef {Object} SharedWithMeItem
 * @property {ResourceTypeEnum}        resource_type
 * @property {PermissionTypeEnum[]}    permissions   - All permissions the caller holds on this resource.
 * @property {string}                  granted_at    - ISO-8601 timestamp of the earliest grant.
 * @property {string}                  granted_by    - UUID of the user who created the grant.
 * @property {FileItem|FolderItem}     resource      - Full resource details; shape follows resource_type.
 */

/**
 * Response for `GET /api/grants/incoming/resources`.
 * @typedef {Object} SharedWithMeResponse
 * @property {SharedWithMeItem[]}  items
 * @property {string|undefined}    [next_cursor]  - Absent when the last page is reached.
 */

/**
 * @typedef {Object} ContactEmail
 * @property {string}  email
 * @property {string}  type        - e.g. "work", "home"
 * @property {boolean} is_primary
 */

/**
 * Mirrors the backend `ContactDto`.
 * `id` equals the OxiCloud user UUID for contacts from the system address book.
 * @typedef {Object} ContactItem
 * @property {string}          id
 * @property {string}          address_book_id
 * @property {string}          uid             - vCard UID
 * @property {string|null}     [full_name]
 * @property {string|null}     [first_name]
 * @property {string|null}     [last_name]
 * @property {string|null}     [nickname]
 * @property {ContactEmail[]}  email
 * @property {string|null}     [organization]
 * @property {string|null}     [title]
 * @property {string|null}     [photo_url]
 * @property {string}          created_at      - ISO-8601
 * @property {string}          updated_at      - ISO-8601
 * @property {string}          etag
 */

/**
 * Mirrors the backend `AddressBookResponse`.
 * @typedef {Object} AddressBookItem
 * @property {string}       id
 * @property {string}       name
 * @property {string}       owner_id
 * @property {string|null}  [description]
 * @property {string|null}  [color]
 * @property {boolean}      is_public
 * @property {boolean}      is_readonly
 * @property {boolean}      is_system
 * @property {string}       created_at   - ISO-8601
 * @property {string}       updated_at   - ISO-8601
 */

// ------------------- share modal

/**
 * Share roles (DTO-layer sugar for the ReBAC permission sets).
 * @typedef {'viewer'|'editor'|'admin'} ShareRoleEnum
 */

/**
 * One collaborator row in the share modal's People section.
 * @typedef {Object} MemberEntry
 * @property {Grant}         grant   - Representative grant (used for subject/resource info).
 * @property {Grant[]}       _grants - All grants for this subject on the resource (may be > 1).
 * @property {ShareRoleEnum} role    - Derived role label shown in the UI.
 * @property {'keep'|'remove'|'change'|'new'} _op - Pending local operation.
 */

/**
 * Existing public link with a pending local operation.
 * @typedef {Object} LinkEntry
 * @property {ShareItem}  share   - The existing share object.
 * @property {'keep'|'remove'|'edit'} _op - Pending local operation.
 * @property {DraftLink|null} _draft - Updated fields when _op === 'edit'.
 */

/**
 * A public link staged for creation (not yet committed).
 * @typedef {Object} DraftLink
 * @property {string}      name
 * @property {string|null} password
 * @property {string|null} expires_at  - ISO-8601 date string or null.
 */

