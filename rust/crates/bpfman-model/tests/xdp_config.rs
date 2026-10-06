//! XDP dispatcher ABI and validated input boundaries.

use bpfman_model::{
    InvalidXdpConfig, XDP_MAX_MEMBERS, XdpConfig, XdpPriority, XdpProceedOn, XdpSlot, xdp_config,
};

#[test]
fn every_supported_capacity_has_the_go_abi_layout() -> Result<(), Box<dyn std::error::Error>> {
    for count in 1..=XDP_MAX_MEMBERS {
        let actions: Vec<_> = (0..count)
            .map(|i| XdpProceedOn::try_from(1 << (i % 5)))
            .collect::<Result<_, _>>()?;
        let config = XdpConfig::new(&actions)?;
        let bytes = config.bytes();
        assert_eq!(&bytes[..4], &[236, 2, count as u8, 0]);
        for slot in 0..10 {
            let offset = 4 + slot * 4;
            let expected = actions.get(slot).map_or(0, |a| a.mask());
            assert_eq!(&bytes[offset..offset + 4], &expected.to_ne_bytes());
            let offset = 44 + slot * 4;
            assert_eq!(&bytes[offset..offset + 4], &50u32.to_ne_bytes());
        }
        assert!(bytes[84..].iter().all(|&b| b == 0));
    }
    assert_eq!(XdpConfig::new(&[]), Err(InvalidXdpConfig::Capacity));
    assert_eq!(
        XdpConfig::new(&[XdpProceedOn::default(); 11]),
        Err(InvalidXdpConfig::Capacity)
    );
    Ok(())
}

#[test]
fn single_member_encoding_preserves_all_valid_action_masks()
-> Result<(), Box<dyn std::error::Error>> {
    for low in 0..32 {
        for high in [0, 1 << 31] {
            let action = XdpProceedOn::try_from(low | high)?;
            assert_eq!(XdpConfig::new(&[action])?, XdpConfig::single(action));
            assert_eq!(*XdpConfig::single(action).bytes(), xdp_config(action));
            assert_eq!(&xdp_config(action)[4..8], &(low | high).to_ne_bytes());
        }
    }
    Ok(())
}

#[test]
fn priority_and_slot_boundaries_are_refined_once() -> Result<(), InvalidXdpConfig> {
    for priority in [0, 1, 50, i32::MAX as u32] {
        assert_eq!(XdpPriority::try_from(priority)?.get(), priority);
    }
    for priority in [i32::MAX as u32 + 1, u32::MAX] {
        assert_eq!(
            XdpPriority::try_from(priority),
            Err(InvalidXdpConfig::Priority)
        );
    }
    for slot in 0..10 {
        assert_eq!(XdpSlot::try_from(slot)?.index(), slot);
    }
    for slot in [10, 256, usize::MAX] {
        assert_eq!(XdpSlot::try_from(slot), Err(InvalidXdpConfig::Slot));
    }
    Ok(())
}
