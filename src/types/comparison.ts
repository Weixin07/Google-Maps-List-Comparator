import type { DriveFileMetadata } from "./drive";

export type ListSlot = "A" | "B";

export type ComparisonStats = {
  list_a_count: number;
  list_b_count: number;
  overlap_count: number;
  only_a_count: number;
  only_b_count: number;
  pending_a: number;
  pending_b: number;
};

export type PlaceComparisonRow = {
  place_id: string;
  name: string;
  formatted_address?: string | null;
  lat: number;
  lng: number;
  types: string[];
  lists: ListSlot[];
};

export type ComparisonProjectInfo = {
  id: number;
  name: string;
};

export type ComparisonSnapshot = {
  project: ComparisonProjectInfo;
  stats: ComparisonStats;
  overlap: PlaceComparisonRow[];
  only_a: PlaceComparisonRow[];
  only_b: PlaceComparisonRow[];
};

export type ComparisonSegmentKey = "overlap" | "only_a" | "only_b";

export type ComparisonProjectRecord = {
  id: number;
  name: string;
  slug: string;
  created_at: string;
  updated_at: string;
  is_active: boolean;
  list_a_imported_at?: string | null;
  list_b_imported_at?: string | null;
  list_a_drive_file?: DriveFileMetadata | null;
  list_b_drive_file?: DriveFileMetadata | null;
};

export type MapStyleDescriptor = {
  style_url?: string | null;
};

export type ExportSummary = {
  path: string;
  rows: number;
  selected: number;
  format: string;
  segment: string;
};
