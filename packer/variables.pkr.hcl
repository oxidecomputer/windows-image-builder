variable "windows_iso_path" {
  type        = string
  description = "Path to the Windows Server ISO."
}

variable "windows_iso_checksum" {
  type        = string
  default     = "none"
  description = "Checksum for the Windows Server ISO (e.g. 'sha256:abc123'). Set to 'none' to skip verification."
}

variable "virtio_iso_path" {
  type        = string
  description = "Path to the VirtIO drivers ISO (e.g. virtio-win.iso from Fedora)."
}

variable "ovmf_code_path" {
  type        = string
  default     = "/usr/share/edk2/x64/OVMF_CODE.4m.fd"
  description = "Path to OVMF UEFI firmware code. Common paths: /usr/share/edk2/x64/OVMF_CODE.4m.fd (Arch), /usr/share/OVMF/OVMF_CODE.fd (Ubuntu), /usr/share/edk2/ovmf/OVMF_CODE.fd (Fedora)."
}

variable "ovmf_vars_path" {
  type        = string
  default     = "/usr/share/edk2/x64/OVMF_VARS.4m.fd"
  description = "Path to OVMF UEFI firmware vars. Common paths: /usr/share/edk2/x64/OVMF_VARS.4m.fd (Arch), /usr/share/OVMF/OVMF_VARS.fd (Ubuntu), /usr/share/edk2/ovmf/OVMF_VARS.fd (Fedora)."
}

variable "disk_size" {
  type        = string
  default     = "30G"
  description = "Size of the output disk image."
}

variable "memory" {
  type        = number
  default     = 4096
  description = "Memory in MB for the build VM."
}

variable "cpus" {
  type        = number
  default     = 4
  description = "Number of vCPUs for the build VM."
}

variable "output_directory" {
  type        = string
  default     = "output"
  description = "Directory for the output image."
}

variable "windows_version" {
  type        = string
  default     = "2k22"
  description = "Windows version for VirtIO driver selection (e.g. 2k16, 2k19, 2k22, 2k25)."

  validation {
    condition     = contains(["2k16", "2k19", "2k22", "2k25"], var.windows_version)
    error_message = "The windows_version must be one of 2k16, 2k19, 2k22, or 2k25."
  }
}

variable "image_index" {
  type        = string
  default     = "2"
  description = "Windows image index to install (e.g. 2 = Standard Desktop Experience)."
}

variable "headless" {
  type        = bool
  default     = true
  description = "Run the build VM without a GUI. Set to false to see the Windows installer."
}
