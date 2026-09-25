/**
 * The platform UI that generated components render through.
 *
 * The components are Zaidan source, copied into this package and owned here.
 * The generator names these exports, and this package owns how they look.
 * The stylesheet is `@wamn/ui/styles.css`.
 */

export * from "./blocks/data-grid";
export { Badge } from "./components/ui/badge";
export { Button } from "./components/ui/button";
export { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./components/ui/card";
export { Field, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "./components/ui/field";
export { Input } from "./components/ui/input";
export { Toaster } from "./components/ui/toast";
export { ColorModeProvider, getClientColorMode, useColorMode } from "./components/color-mode";
export {
  FormActions,
  FormDone,
  TableScreen,
  type FormActionsProps,
  type FormDoneProps,
  type TableScreenProps,
} from "./actions";
export { ConfirmAction, type ConfirmActionProps } from "./confirm";
export { DetailItem, DetailList, type DetailItemProps, type DetailListProps } from "./detail";
export {
  CheckField,
  ChoiceField,
  TextField,
  type CheckFieldProps,
  type Choice,
  type ChoiceFieldProps,
  type TextFieldProps,
} from "./fields";
export {
  AppFrame,
  CardPage,
  ScreenActions,
  type AppFrameProps,
  type CardPageProps,
  type FrameEntry,
  type FrameItem,
  type FrameLink,
  type FrameSection,
  type ScreenActionsProps,
} from "./frame";
export { gridFeatures, type GridFeatures } from "./grid";
export { announceOutcome } from "./outcome";
export { createRecordLabels } from "./record-labels";
export { RecordSelect, type RecordSelectProps } from "./record-select";
export { ROW_HEIGHT, WINDOW_FROM, WindowedTable } from "./windowed-table";
export { type DataTableFilter } from "./table/column-filter";
export { DataTable, type DataTableColumn, type DataTableColumnRole, type DataTableColumnType, type DataTableProps, type DataTableSort, type DataTableSortDirection } from "./table/data-table";
export { type DataTableAggregate, type DataTableBucket } from "./table/aggregate";
export { type DataTableGroupSort } from "./table/group-bar";
