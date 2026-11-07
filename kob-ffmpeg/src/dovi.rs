use crate::ffi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DolbyVision {
    pub profile: u8,
    pub level: u8,
    pub bl_signal_compatibility_id: u8,
}

impl DolbyVision {
    /// # Safety
    /// `params` must be null or point to a valid `AVCodecParameters`.
    pub unsafe fn from_codec_parameters(params: *const ffi::AVCodecParameters) -> Option<Self> {
        if params.is_null() {
            return None;
        }
        unsafe {
            let sd = ffi::av_packet_side_data_get(
                (*params).coded_side_data,
                (*params).nb_coded_side_data,
                ffi::AV_PKT_DATA_DOVI_CONF,
            );
            if sd.is_null()
                || (*sd).size < std::mem::size_of::<ffi::AVDOVIDecoderConfigurationRecord>()
            {
                return None;
            }
            let record = &*((*sd).data as *const ffi::AVDOVIDecoderConfigurationRecord);
            Some(Self {
                profile: record.dv_profile,
                level: record.dv_level,
                bl_signal_compatibility_id: record.dv_bl_signal_compatibility_id,
            })
        }
    }

    /// Profile 5 (and any stream signalling compatibility id 0) codes its base
    /// layer in IPTPQc2, which only looks right after the RPU reshapes it.
    pub fn has_compatible_base_layer(&self) -> bool {
        self.bl_signal_compatibility_id != 0
    }

    pub fn label(&self) -> String {
        if self.profile == 8 {
            format!(
                "Dolby Vision P{}.{}",
                self.profile, self.bl_signal_compatibility_id
            )
        } else {
            format!("Dolby Vision P{}", self.profile)
        }
    }
}
