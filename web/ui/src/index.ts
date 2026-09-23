/**
 * The platform UI that generated components render through.
 *
 * The components are Zaidan source, copied into this package and owned here.
 * The generator names these exports, and this package owns how they look.
 * The stylesheet is `@wamn/ui/styles.css`.
 */

export * from "./blocks/data-grid";
export { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger } from "./components/ui/alert-dialog";
export { Badge } from "./components/ui/badge";
export { Button } from "./components/ui/button";
export { Checkbox } from "./components/ui/checkbox";
export { Field, FieldDescription, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "./components/ui/field";
export { Input } from "./components/ui/input";
export { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./components/ui/select";
export { Skeleton } from "./components/ui/skeleton";
export { Toaster } from "./components/ui/toast";
export { ColorModeProvider, getClientColorMode, useColorMode } from "./components/color-mode";
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
export { gridFeatures, type GridFeatures } from "./grid";
export { announceOutcome } from "./outcome";
export { RecordSelect, SEARCH_PAUSE_MS, type RecordSelectProps } from "./record-select";
