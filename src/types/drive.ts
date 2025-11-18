export type DriveFileMetadata = {
  id: string;
  name: string;
  mime_type: string;
  modified_time?: string | null;
  size?: number | null;
};
